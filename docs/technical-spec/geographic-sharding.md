# Geographic Sharding Specification

This document specifies the complete sharding system for Gratia, including shard assignment, scaling, merge/split criteria, cross-shard transactions, security hardening, and attack mitigations. It addresses Threat Model §2.3 (Geographic Shard Attack).

---

## Design Tension

Geographic sharding exists for **performance** — keeping nearby nodes in the same shard minimizes consensus latency and ensures most transactions (local payments, merchant interactions) stay within a single shard.

But geographic sharding creates a **security risk** — an attacker can concentrate nodes in a small shard and approach majority. Every design decision in this spec navigates this tension.

**Resolution principle:** Performance is the default. Security overrides performance when a shard is under threat. The system degrades gracefully from fast-and-sharded to slower-and-merged rather than allowing a compromised shard.

---

## Shard Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                    BEACON CHAIN                          │
│  Global state root, shard registry, cross-shard proofs  │
├────────┬────────┬────────┬────────┬────────┬────────────┤
│Shard 0 │Shard 1 │Shard 2 │Shard 3 │  ...   │ Shard N-1  │
│Americas│W.Europe│E.Europe│  Asia  │        │  Oceania   │
│  West  │+Africa │+M.East │Pacific │        │            │
├────────┴────────┴────────┴────────┴────────┴────────────┤
│              Cross-Shard Relay Layer                     │
│     Merkle proofs, receipts, periodic checkpoints       │
└─────────────────────────────────────────────────────────┘
```

Each shard:
- Runs its own validator committee (sized per the graduated committee spec)
- Processes transactions originating from nodes in its geographic region
- Maintains its own state trie, block height, and state root
- Checkpoints its state root to the beacon chain every epoch (100 blocks)

---

## Shard Assignment

### Primary: Geographic (Longitude + Latitude Bands)

The current implementation divides the globe into equal longitudinal bands. This is insufficient — it creates shards of wildly different population density (a shard covering the Pacific Ocean vs. one covering India).

**New approach: population-weighted geographic regions.**

Phase 1 uses fixed regions aligned to continental boundaries:

| Shard ID | Region | Approximate Coverage |
|----------|--------|---------------------|
| 0 | Americas West | Western Hemisphere, lon < -30° |
| 1 | Americas East + West Africa | -30° to 0° |
| 2 | Europe + Central Africa | 0° to 40° |
| 3 | Middle East + East Africa + Central Asia | 40° to 70° |
| 4 | South + Southeast Asia | 70° to 105° |
| 5 | East Asia | 105° to 135° |
| 6 | Oceania + Pacific | 135° to 180° / -180° to -150° |

<!-- WHY: Seven regions rather than equal longitude bands. These boundaries roughly
     equalize expected smartphone population per shard. India and Southeast Asia
     (the highest smartphone density regions for Gratia's target demographic) are
     split across shards 4 and 5 to prevent one mega-shard. -->

Starting with 4 active shards at genesis (merging low-population regions), expanding to 7 as the network grows, and eventually splitting high-density shards further up to MAX_SHARDS (20).

### Secondary: Random Rotation Component

To mitigate geographic concentration attacks, **20% of each shard's validator committee is drawn from neighboring shards.** This means an attacker flooding a single region still faces validators from outside their controlled area.

Implementation:
- Each shard's committee has `committee_size` slots
- `floor(committee_size × 0.8)` slots are filled by VRF selection from local nodes
- `ceil(committee_size × 0.2)` slots are filled by VRF selection from nodes in adjacent shards
- Adjacent shards are determined by the shard registry (geographic neighbors, not just ±1 ID)

Example at full scale (21-validator committee):
- 17 local validators + 4 cross-shard validators
- An attacker controlling 50% of local nodes still only controls ~8.5 of 21 seats on average — below the stall threshold

<!-- WHY: 20% cross-shard is the sweet spot. Higher percentages increase latency
     (validators validating transactions from a foreign region). Lower percentages
     don't provide meaningful security. 20% adds ~50-100ms latency for the cross-shard
     validators while preventing single-shard capture. -->

### Fallback: Hash-Based Assignment

Nodes without a GPS fix (rare — GPS is a core PoL requirement) are assigned by address hash as in the current implementation. These nodes are distributed evenly across shards and do not count toward any shard's geographic concentration metrics.

---

## Shard Scaling: Split and Merge

### Split Criteria (Shard → Two Shards)

A shard splits when ALL of the following are true for 30 consecutive days:
1. Active nodes > 5× `min_nodes_per_shard` (currently: > 5,000 nodes)
2. Total active shards < `MAX_SHARDS` (20)
3. Transaction throughput in the shard exceeds 70% of per-shard capacity for 7+ days
4. The split would produce two sub-shards each with > 2× `min_nodes_per_shard`

**Split procedure:**
1. The beacon chain announces the pending split with the new boundary (chosen to equalize node count between sub-shards)
2. 7-day notice period — nodes prepare for reassignment
3. At the epoch boundary after the notice period, the shard forks into two new shards
4. Each sub-shard inherits the relevant portion of the parent shard's state trie
5. Cross-shard receipts are generated for any in-flight transactions that now span the new boundary
6. Both sub-shards checkpoint to the beacon chain within the first epoch

<!-- WHY: The 30-day sustained requirement prevents thrashing from temporary node spikes.
     The 5× minimum ensures both sub-shards are well above the safety threshold.
     The 70% throughput trigger means we only split when there's actual capacity pressure,
     not just because a region has many nodes. -->

### Merge Criteria (Two Shards → One)

A shard merges with its geographic neighbor when ANY of the following are true:
1. Active nodes < `min_nodes_per_shard` for 7 consecutive days
2. Active nodes < 2× `min_nodes_per_shard` AND the shard has been flagged for security concerns (see Shard Health Monitoring)
3. Governance vote (emergency merge for security reasons)

**Merge procedure:**
1. The beacon chain announces the pending merge with the target neighbor shard
2. 3-day notice period (shorter than split — merge is a safety action)
3. At the epoch boundary, the smaller shard's state trie is merged into the neighbor's
4. All nodes from the dissolved shard are reassigned to the merged shard
5. The merged shard's committee expands to accommodate the additional nodes

<!-- WHY: Merge threshold is 7 days (not 30 like split) because under-populated shards
     are an active security risk. A shard with 40 nodes is dangerously close to
     committee capture. Fast merge protects those users. -->

### Minimum Nodes Per Shard

| Network Phase | Min Nodes Per Shard | Rationale |
|--------------|-------------------|-----------|
| Bootstrap (< 10K total) | No sharding — single global chain | Not enough nodes for safe shard isolation |
| Early (10K-50K total) | 1,000 | With graduated committee (11 validators), need 1,000+ for safe VRF selection pool |
| Established (50K-250K) | 2,500 | Full 19-21 validator committees, need depth for security |
| Mature (250K+) | 5,000 | Standard operating minimum with full attack mitigations |

<!-- WHY: The minimum is deliberately high — higher than what consensus technically requires
     — because shard security is multiplicatively weaker than global chain security.
     An attacker targeting one shard faces 1/N of the total network defense. The minimum
     must be high enough that even a single-shard attack is expensive. -->

**Critical rule: sharding does NOT activate until the global network exceeds 10,000 nodes.** Below that threshold, all nodes participate in a single global chain. The performance is sufficient (131-218 TPS) for a network of that size, and the security benefit of keeping everyone in one pool outweighs the latency benefit of sharding.

---

## Cross-Shard Transactions

### Transaction Flow

```
1. Alice (Shard 2, Europe) sends 50 GRAT to Bob (Shard 4, SE Asia)

2. Alice's shard processes the send:
   - Deducts 50 GRAT from Alice's account
   - Creates a CrossShardReceipt with merkle proof of the deduction
   - Includes the receipt in the shard's block

3. At the next beacon chain checkpoint:
   - Shard 2's state root (including the receipt) is posted to beacon chain
   - Shard 4 sees the receipt via the beacon chain

4. Shard 4 processes the receive:
   - Verifies the merkle proof against Shard 2's state root on the beacon chain
   - Credits 50 GRAT to Bob's account
   - Marks the receipt as consumed

5. Finality:
   - Send is final when Shard 2 checkpoints to beacon chain (~1-2 epochs, ~3-16 minutes)
   - Credit is final when Shard 4 processes the receipt (~1 additional epoch)
   - Total cross-shard finality: ~5-20 minutes
```

### Latency Comparison

| Transaction Type | Finality Time | Notes |
|-----------------|--------------|-------|
| Same-shard | 3-5 seconds | Single block confirmation |
| Cross-shard | 5-20 minutes | Depends on checkpoint timing |
| NFC tap-to-pay (same shard) | 3-5 seconds | Optimistic confirmation possible |

<!-- WHY: Cross-shard latency is acceptable because most phone-based transactions are local.
     A person buying coffee, paying a merchant, or sending money to a friend nearby will
     almost always be in the same shard. Cross-shard transfers are more like bank wires
     than tap-to-pay — they can tolerate minutes of latency. -->

### Cross-Shard Fee

Cross-shard transactions incur a small additional fee (burned, not paid to validators) to:
1. Compensate for the additional verification work across two shards
2. Discourage unnecessary cross-shard traffic
3. Provide a natural incentive for users to transact locally

Fee amount: governance-adjustable, initially set at 2× the standard transaction fee.

---

## Shard Security Hardening

### 1. Cross-Shard Validator Rotation (20% Rule)

Described above in Shard Assignment. The 20% cross-shard committee seats ensure no single-shard capture can achieve finality.

### 2. Cross-Shard Block Auditing

Every epoch (100 blocks), each shard's finalized blocks are audited by a random subset of nodes from **two other shards.** The audit verifies:
- Block signatures are valid
- State transitions are consistent
- No double-spends within the shard
- Slashing events were applied correctly

If auditors find an invalid block, they submit a **fraud proof** to the beacon chain. The beacon chain halts the offending shard and triggers an investigation (governance process).

Audit is lightweight — auditors verify merkle proofs and signatures, not re-execute every transaction.

<!-- WHY: Cross-shard auditing is the primary defense against a captured shard producing
     invalid blocks. Even if an attacker controls 100% of a shard's committee (nearly
     impossible with the 20% cross-shard rule), the audit from other shards catches it
     within one epoch. -->

### 3. Shard Health Monitoring

The beacon chain tracks per-shard health metrics:

| Metric | Healthy | Warning | Critical |
|--------|---------|---------|----------|
| Active nodes | > 2× min | 1-2× min | < min (triggers merge) |
| Finality rate | > 95% of blocks finalize | 80-95% | < 80% (possible stall attack) |
| Validator diversity | Top entity < 20% of committee slots | 20-33% | > 33% (possible capture) |
| Cross-shard audit pass rate | 100% | < 100% (investigation) | N/A |
| Node churn rate | < 5%/week | 5-15%/week | > 15%/week (instability) |

**Warning** triggers: heightened PoL scrutiny for all nodes in the shard, increased audit frequency (every 50 blocks instead of 100).

**Critical** triggers: immediate merge consideration, beacon chain alert to all nodes, governance notification.

### 4. Shard Assignment Jitter

To prevent an attacker from precisely predicting which shard a node will land in:
- Shard assignment includes a small random jitter: nodes within 2° of a shard boundary have a 30% chance of being assigned to the adjacent shard
- The jitter seed is derived from the node's address + current epoch, making it deterministic (all nodes agree) but unpredictable to the attacker before the node registers

This means an attacker positioning nodes right at a shard boundary cannot guarantee which shard they'll end up in, complicating geographic concentration attacks.

### 5. Emergency Shard Freeze

If a fraud proof is confirmed against a shard:
1. The shard is immediately frozen — no new blocks are produced
2. Cross-shard transactions to/from the frozen shard are halted
3. The beacon chain appoints an emergency committee from high-Presence-Score nodes across ALL shards
4. The emergency committee re-validates the shard's recent blocks and identifies the point of divergence
5. The shard is rolled back to the last valid state and restarted with a fresh committee
6. All validators who signed invalid blocks are slashed (Critical — 100% burn + permanent ban)

<!-- WHY: Shard freeze is the nuclear option. It's disruptive to users in the affected region
     but protects the integrity of the global network. The alternative — letting a captured
     shard produce invalid state that propagates cross-shard — is far worse. -->

---

## Beacon Chain

The beacon chain is a lightweight coordination layer, NOT a full execution chain:

**What it stores:**
- Shard registry (active shards, boundaries, node counts)
- Per-shard state roots (checkpointed every epoch)
- Cross-shard receipts (pending and consumed)
- Shard health metrics
- Fraud proofs and emergency actions

**What it does NOT store:**
- Individual transactions (those live in their shard)
- Account balances (those live in their shard's state trie)
- Smart contract state (shard-local)

**Who validates it:**
- The beacon chain has its own validator committee, selected via VRF from the top 10% Presence Score nodes across ALL shards
- Beacon committee size: same as the graduated committee spec, based on total network size
- Beacon chain block time: 10 seconds (slower than shard blocks — coordination doesn't need speed)

<!-- WHY: Separating the beacon chain from shard execution keeps it lightweight. The beacon
     chain's state is small (shard roots + registry) and can be validated by any node
     regardless of which shard they belong to. This prevents the beacon chain from
     becoming a bottleneck. -->

---

## Shard Lifecycle Summary

```
Network < 10K nodes:
  └── Single global chain (no sharding)

Network hits 10K:
  └── Activate 4 initial shards (Americas, Europe+Africa, Asia, Oceania)
      Each shard: ~2,500 nodes, committee sized per graduated spec

Shard exceeds 5× minimum for 30 days + throughput pressure:
  └── Split into 2 sub-shards along population-weighted boundary

Shard drops below minimum for 7 days:
  └── Merge with geographic neighbor

Shard health critical:
  └── Merge immediately OR emergency freeze if fraud detected

Network at scale (1M+ nodes):
  └── Up to 20 shards, each with 50K+ nodes
      Full 21-validator committees with 20% cross-shard rotation
      Cross-shard auditing every epoch
      Beacon chain coordinating state roots
```

---

## Parameters Summary

| Parameter | Value | Adjustable |
|-----------|-------|-----------|
| MAX_SHARDS | 20 | Governance vote |
| Initial shards at activation | 4 | Hardcoded for genesis |
| Sharding activation threshold | 10,000 total nodes | Governance vote |
| Min nodes per shard (mature) | 5,000 | Governance vote |
| Split threshold | 5× min for 30 days + 70% throughput | Governance vote |
| Merge threshold | < min for 7 days | Governance vote |
| Cross-shard committee % | 20% | Governance vote |
| Beacon checkpoint interval | 100 blocks (~5-8 min) | Governance vote |
| Cross-shard audit interval | 100 blocks (50 if shard in warning) | Governance vote |
| Shard boundary jitter | 2° / 30% probability | Hardcoded |
| Cross-shard fee multiplier | 2× standard fee | Governance vote |

---

## Impact on Threat Model §2.3

The geographic shard attack (flooding a small shard with attacker nodes) is now mitigated at multiple layers:

1. **No sharding below 10K nodes** — eliminates the attack during the most vulnerable phase
2. **High minimum nodes per shard (5,000)** — the attacker needs thousands of phones to approach majority in any shard
3. **20% cross-shard validators** — even with local majority, 4 of 21 validators are from outside the region
4. **Cross-shard auditing** — invalid blocks are caught within one epoch by external auditors
5. **Shard health monitoring** — concentration is detected and triggers automatic responses
6. **Boundary jitter** — attacker can't precisely control which shard their nodes land in
7. **Emergency freeze** — if all else fails, the shard is frozen and rolled back

An attacker targeting a single shard of 5,000 nodes needs:
- ~2,500 phones in the region for local majority → ~$1.27M/year operating cost
- But 20% cross-shard validators mean they still only control ~80% × 67% ≈ 53% of committee seats on average
- Cross-shard auditors catch any invalid blocks within minutes
- Shard health monitoring flags the concentration within days

**Net assessment:** Geographic shard attack is reduced from **High** severity to **Low-Medium** with this specification implemented.

---

## Open Questions

- [ ] Exact initial shard boundaries — need population density data for smartphone users in target demographics to optimize the 7-region split
- [ ] Beacon chain validator incentives — should beacon validators earn a premium for the coordination role?
- [ ] Cross-shard smart contract calls — how does a contract in Shard 2 read state from Shard 4? (Likely: async message passing with callback, not synchronous reads)
- [ ] Shard rebalancing — if organic growth creates a 50K-node shard next to a 5K-node shard, should nodes be periodically reassigned? (Tension with geographic locality)
