# Graduated Committee Scaling Specification

This document defines how the validator committee size scales with network size to protect the early network from committee capture attacks.

---

## The Problem

The standard design calls for a 21-validator committee with 14/21 (67%) finality threshold. At full network scale (100K+ nodes), this is statistically safe — an attacker with 10% of nodes has near-zero probability of capturing 14 seats.

But at launch with 1,000 nodes, a 21-validator committee is **dangerous:**
- An attacker with 100 nodes (10%) has a small but real probability of landing 8+ seats (stall)
- An attacker with 200 nodes (20%) has a meaningful probability of landing 8+ seats
- Committee rotation every few seconds means the attacker gets thousands of attempts per day

The solution: **scale the committee with the network**, using a smaller committee when the network is small.

---

## Why Smaller Committees Are Safer at Low Node Counts

This is counterintuitive. A smaller committee seems like it would be easier to capture. But the math works differently when we consider the **finality threshold as a fraction**:

For a committee of size C with finality threshold F (always 67%):
- Finality requires ⌈C × 0.67⌉ seats
- Stalling requires C − ⌈C × 0.67⌉ + 1 seats

The key insight: **with fewer total committee slots, VRF selection from a smaller pool is more representative.** An attacker with 10% of the pool is very unlikely to get 67% of even a small committee, because the VRF is selecting from a pool where they're a small minority.

What actually protects the network is the **ratio of attacker nodes to total nodes**, not the committee size. A smaller committee with the same selection pool produces similar capture probabilities — but with less overhead and faster finality in a small network.

The real benefit of graduated scaling is **matching committee overhead to network capacity** while maintaining the security invariant.

---

## Scaling Curve

| Network Size (Total Nodes) | Committee Size | Finality Threshold | Stall Threshold | Min Nodes for Selection Pool |
|---------------------------|---------------|-------------------|-----------------|----------------------------|
| < 100 | 3 | 2/3 (67%) | 2/3 (67%) | 10 |
| 100 – 499 | 5 | 4/5 (80%) | 2/5 (40%) | 50 |
| 500 – 2,499 | 7 | 5/7 (71%) | 3/7 (43%) | 100 |
| 2,500 – 9,999 | 11 | 8/11 (73%) | 4/11 (36%) | 500 |
| 10,000 – 49,999 | 15 | 10/15 (67%) | 6/15 (40%) | 2,000 |
| 50,000 – 99,999 | 19 | 13/19 (68%) | 7/19 (37%) | 10,000 |
| 100,000+ | 21 | 14/21 (67%) | 8/21 (38%) | 20,000 |

<!-- WHY: The scaling uses odd numbers exclusively to prevent tie conditions in voting.
     Finality threshold stays near 67% at every level for consistency. The curve is
     deliberately conservative — the committee stays small until the network has significant
     depth in its selection pool. -->

### Transition Rules

- Committee size changes take effect at the **next epoch boundary** (not mid-epoch)
- An epoch is defined as 100 blocks (~5-8 minutes at 3-5 second block times)
- Network size is measured as the count of nodes with valid PoL in the last 48 hours
- Transitions only occur **upward** — if node count temporarily dips below a threshold, the committee does not shrink back down unless the dip persists for 7+ days

<!-- WHY: Upward-only with delayed downward prevents an attacker from knocking nodes offline
     to force a committee shrink, which would make capture easier. The 7-day persistence
     requirement means only genuine network contraction triggers downsizing. -->

---

## Committee Capture Probabilities

Using hypergeometric distribution: P(X ≥ k) where X is attacker seats in a committee of C, drawn from a pool of N total nodes with A attacker nodes.

### At 5% Attacker Penetration

| Network Size | Committee | P(Stall: ≥stall_threshold) | P(Capture: ≥finality) | Assessment |
|-------------|-----------|---------------------------|----------------------|-----------|
| 100 | 3 | 0.7% | 0.01% | Safe |
| 500 | 7 | 0.08% | ~0% | Safe |
| 2,500 | 11 | 0.03% | ~0% | Safe |
| 10,000 | 15 | 0.002% | ~0% | Safe |
| 50,000 | 19 | ~0% | ~0% | Safe |
| 100,000 | 21 | ~0% | ~0% | Safe |

### At 10% Attacker Penetration

| Network Size | Committee | P(Stall) | P(Capture) | Assessment |
|-------------|-----------|----------|------------|-----------|
| 100 | 3 | 2.8% | 0.1% | Acceptable |
| 500 | 7 | 0.5% | ~0% | Safe |
| 2,500 | 11 | 0.2% | ~0% | Safe |
| 10,000 | 15 | 0.03% | ~0% | Safe |
| 50,000 | 19 | ~0% | ~0% | Safe |
| 100,000 | 21 | ~0% | ~0% | Safe |

### At 20% Attacker Penetration

| Network Size | Committee | P(Stall) | P(Capture) | Assessment |
|-------------|-----------|----------|------------|-----------|
| 100 | 3 | 10.4% | 0.8% | **Elevated risk** |
| 500 | 7 | 3.3% | 0.04% | Acceptable |
| 2,500 | 11 | 1.6% | 0.003% | Safe |
| 10,000 | 15 | 0.4% | ~0% | Safe |
| 50,000 | 19 | 0.05% | ~0% | Safe |
| 100,000 | 21 | 0.01% | ~0% | Safe |

### At 33% Attacker Penetration (Consensus Threat Level)

| Network Size | Committee | P(Stall) | P(Capture) | Assessment |
|-------------|-----------|----------|------------|-----------|
| 100 | 3 | 26% | 3.7% | **Dangerous** |
| 500 | 7 | 12% | 0.5% | **Elevated risk** |
| 2,500 | 11 | 7.5% | 0.1% | Elevated but manageable |
| 10,000 | 15 | 3.5% | 0.02% | Acceptable |
| 50,000 | 19 | 1.2% | ~0% | Safe |
| 100,000 | 21 | 0.5% | ~0% | Safe |

---

## Early Network Special Rules (< 2,500 nodes)

The first phase of the network has additional protections beyond the graduated committee:

### 1. Founding Node Density

The founding team operates real phones under the same protocol rules as everyone else. Target: **founding nodes ≥ 50% of total network** during the first 6 months.

This is not centralization — founding nodes:
- Run the same software
- Follow the same PoL rules
- Have no special privileges
- Cannot override governance
- Are subject to the same slashing

They simply provide a density of known-honest nodes that makes 33% attacker penetration require overwhelming the founding fleet. At 500 founding nodes, an attacker needs 250+ phones to reach 33% of the remaining non-founding pool — and founding nodes still dilute their overall percentage.

<!-- WHY: Every blockchain has a bootstrapping period where the founding team is a significant
     fraction of the network. Bitcoin was mined almost exclusively by Satoshi for months.
     The difference is Gratia is transparent about it and designs for it. -->

### 2. Cooldown Between Committee Selections

At < 2,500 nodes, the same node **cannot be selected for committee in consecutive rounds.** This prevents a small set of attacker nodes from appearing in back-to-back committees.

- At committee size 3 with 100 nodes: cooldown of 5 rounds
- At committee size 5 with 300 nodes: cooldown of 3 rounds
- At committee size 7 with 1,000 nodes: cooldown of 2 rounds
- At ≥ 2,500 nodes: cooldown of 1 round (standard)

### 3. Statistical Monitoring

The protocol tracks committee selection statistics. If any node's selection frequency exceeds 3 standard deviations above expected, it is flagged for enhanced PoL verification.

Expected selection frequency for an honest node: `committee_size / total_eligible_nodes` per round.

If a node is consistently over-selected (which shouldn't happen with fair VRF, but could indicate VRF manipulation), the anomaly is visible to all nodes and triggers investigation.

### 4. Minimum Selection Pool

Each committee size has a minimum selection pool (see scaling table). If eligible nodes fall below this minimum, the committee shrinks to the next smaller size. This ensures the VRF always has enough candidates for a statistically meaningful selection.

---

## Implementation Notes

### Committee Selection Algorithm

```
fn select_committee(eligible_nodes: &[Node], network_size: usize) -> Vec<Node> {
    let committee_size = match network_size {
        0..=99       => 3,
        100..=499    => 5,
        500..=2499   => 7,
        2500..=9999  => 11,
        10000..=49999 => 15,
        50000..=99999 => 19,
        _            => 21,
    };

    let finality_threshold = match committee_size {
        3  => 2,
        5  => 4,
        7  => 5,
        11 => 8,
        15 => 10,
        19 => 13,
        21 => 14,
        _  => unreachable!(),
    };

    // VRF-weighted selection from eligible pool
    // (existing ECVRF implementation, just parameterized by committee_size)
    vrf_select(eligible_nodes, committee_size, current_block_hash)
}
```

### State Transitions

The `gratia-consensus` crate's `committee.rs` module needs:

1. A `CommitteeConfig` struct parameterized by network size
2. The scaling curve as a const lookup table
3. Epoch boundary detection for committee size transitions
4. Cooldown tracking per node (ring buffer of recent committee members)
5. Anomaly detection for selection frequency

### Consensus Message Changes

Block headers must include:
- `committee_size: u8` — the committee size for this block's round
- `finality_threshold: u8` — required votes for finality
- `network_size_snapshot: u32` — the node count used to determine committee size

This allows any validator to independently verify that the correct committee parameters were used.

---

## Summary

| Network Phase | Nodes | Committee | Finality | Primary Defense |
|--------------|-------|-----------|----------|-----------------|
| Bootstrap | < 100 | 3 of 3 | 2/3 | Founding node density + cooldowns |
| Early | 100-499 | 5 of 5 | 4/5 | Founding nodes + statistical monitoring |
| Growing | 500-2,499 | 7 of 7 | 5/7 | Transitioning to organic defense |
| Established | 2,500-9,999 | 11 of 11 | 8/11 | Approaching security threshold |
| Secure | 10,000-49,999 | 15 of 15 | 10/15 | Standard protocol defense |
| Mature | 50,000-99,999 | 19 of 19 | 13/19 | Full defense operational |
| Full scale | 100,000+ | 21 of 21 | 14/21 | Security threshold reached |

The graduated system ensures the network is defensible at every stage of growth, with protections that strengthen as the node count increases and organically replace the founding team's bootstrapping role.
