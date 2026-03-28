# Gig Economy Sybil Attack — Economic Model

This document mathematically models the gig economy Sybil attack (Threat Model §2.1) to determine the **security threshold** — the minimum honest network size at which the attack becomes economically irrational.

---

## The Attack

An attacker pays real humans in low-income regions to carry attacker-controlled phones and use them normally throughout the day. The carriers pass Proof of Life legitimately. The attacker controls the wallet keys and collects mining rewards.

This is the **only known attack that passes all three walls simultaneously.**

---

## Attacker Cost Model

### Per-Phone Costs

| Component | Cost | Notes |
|-----------|------|-------|
| Phone hardware | $50 | Budget Android, one-time |
| Carrier labor | $1/day | Low-income region daily rate |
| Electricity | $0.0024/day | 8hr mining session at global avg |
| SIM / connectivity | $0.10/day | Prepaid data or shared Wi-Fi |
| Logistics overhead | $0.15/day | Management, distribution, replacement |
| **Daily operating cost per phone** | **$1.2524/day** | |
| **Annual operating cost per phone** | **$457/year** | |
| **Total Year 1 cost per phone (incl. hardware)** | **$507** | |

### Stake Capital Requirement

Each phone requires minimum stake. This is locked capital, not spent — but it's at risk of slashing.

| Network Size (honest + attacker) | Min Stake Per Node | Stake Per Attacker Phone |
|----------------------------------|-------------------|-------------------------|
| 10,000 total nodes | ~8,148 GRAT | 8,148 GRAT |
| 50,000 total nodes | ~1,630 GRAT | 1,630 GRAT |
| 100,000 total nodes | ~812 GRAT | 812 GRAT |
| 500,000 total nodes | ~162 GRAT | 162 GRAT |
| 1,000,000 total nodes | ~81 GRAT | 81 GRAT |

<!-- WHY: The self-adjusting 14-day peg means stake cost scales inversely with network size.
     An attacker entering a small network pays more per phone in stake than one entering a
     large network. This is the correct incentive: small networks need more protection. -->

### Fleet Cost Summary

| Fleet Size | Year 1 OpEx | Hardware | Total Year 1 (excl. stake) |
|------------|-------------|----------|---------------------------|
| 1,000 phones | $457,000 | $50,000 | $507,000 |
| 5,000 phones | $2,285,000 | $250,000 | $2,535,000 |
| 10,000 phones | $4,570,000 | $500,000 | $5,070,000 |
| 25,000 phones | $11,425,000 | $1,250,000 | $12,675,000 |
| 50,000 phones | $22,850,000 | $2,500,000 | $25,350,000 |

---

## Attacker Revenue Model

The attacker's revenue depends on two variables:
1. **Share of total mining minutes** — what fraction of the daily budget their fleet captures
2. **Token price** — the USD value of earned GRAT

### Mining Revenue Formula

```
attacker_daily_grat = daily_budget × (attacker_phones / total_miners)
attacker_annual_grat = attacker_daily_grat × 365
attacker_annual_usd = attacker_annual_grat × grat_price
```

Year 1 daily budget = 5,822,000 GRAT (assuming 8hr mining sessions for all miners).

### Revenue Tables

**At $0.001 per GRAT (very early market — ~$2.1M market cap at end of Y1):**

| Attacker Phones | Honest Miners | Attacker % | Annual GRAT Earned | Annual USD Revenue |
|----------------|---------------|------------|-------------------|-------------------|
| 1,000 | 9,000 | 10% | 212,500,000 | $212,500 |
| 5,000 | 45,000 | 10% | 212,500,000 | $212,500 |
| 10,000 | 90,000 | 10% | 212,500,000 | $212,500 |
| 1,000 | 99,000 | 1% | 21,250,000 | $21,250 |
| 10,000 | 990,000 | 1% | 21,250,000 | $21,250 |

<!-- WHY: At the same attacker percentage, revenue is identical regardless of fleet size —
     the daily budget is fixed. A larger fleet in a larger network earns the same fraction. -->

**At $0.01 per GRAT (~$21M market cap):**

| Attacker Phones | Honest Miners | Attacker % | Annual GRAT Earned | Annual USD Revenue |
|----------------|---------------|------------|-------------------|-------------------|
| 1,000 | 9,000 | 10% | 212,500,000 | $2,125,000 |
| 5,000 | 45,000 | 10% | 212,500,000 | $2,125,000 |
| 10,000 | 90,000 | 10% | 212,500,000 | $2,125,000 |
| 1,000 | 99,000 | 1% | 21,250,000 | $212,500 |
| 10,000 | 990,000 | 1% | 21,250,000 | $212,500 |

**At $0.10 per GRAT (~$213M market cap):**

| Attacker Phones | Honest Miners | Attacker % | Annual GRAT Earned | Annual USD Revenue |
|----------------|---------------|------------|-------------------|-------------------|
| 1,000 | 9,000 | 10% | 212,500,000 | $21,250,000 |
| 5,000 | 45,000 | 10% | 212,500,000 | $21,250,000 |
| 10,000 | 90,000 | 10% | 212,500,000 | $21,250,000 |
| 1,000 | 99,000 | 1% | 21,250,000 | $2,125,000 |
| 10,000 | 990,000 | 1% | 21,250,000 | $2,125,000 |

---

## Breakeven Analysis

The critical question: **at what token price does the attack become profitable?**

### Breakeven Formula

```
breakeven_price = attacker_annual_cost / attacker_annual_grat
```

### Breakeven Price by Fleet Size and Network Penetration

| Fleet | Honest Miners | Attacker % | Annual Cost | Annual GRAT | Breakeven Price |
|-------|---------------|------------|-------------|-------------|-----------------|
| 1,000 | 9,000 | 10% | $507,000 | 212,500,000 | **$0.0024** |
| 1,000 | 99,000 | 1% | $507,000 | 21,250,000 | **$0.024** |
| 5,000 | 45,000 | 10% | $2,535,000 | 212,500,000 | **$0.012** |
| 5,000 | 495,000 | 1% | $2,535,000 | 21,250,000 | **$0.119** |
| 10,000 | 90,000 | 10% | $5,070,000 | 212,500,000 | **$0.024** |
| 10,000 | 990,000 | 1% | $5,070,000 | 21,250,000 | **$0.239** |
| 25,000 | 225,000 | 10% | $12,675,000 | 212,500,000 | **$0.060** |
| 25,000 | 2,475,000 | 1% | $12,675,000 | 21,250,000 | **$0.597** |

### Key Insight: The Attacker's Dilemma

The attacker faces a fundamental tradeoff:

1. **Small fleet, small network (high %)** — Cheap to operate, captures a large share. BUT the network is small, so the token likely has low value. Breakeven price is low ($0.002-$0.024), but the token may not even reach that price with only 10K-100K users.

2. **Large fleet, large network (maintain high %)** — Expensive to operate ($5M-$25M/year). Captures the same share. The token is more likely to be worth something, but the cost is enormous and detection risk scales with fleet size.

3. **Large fleet, large network (low %)** — The most realistic scenario for a mature network. The attacker's share is diluted. Breakeven price rises above $0.10-$0.60, requiring significant real-world token demand to be profitable.

**The attacker cannot control both their percentage AND the token price.** A high percentage requires either a small network (low token value) or a massive fleet (high cost). A high token price requires a large, healthy network where the attacker's percentage is naturally diluted.

---

## Security Threshold Determination

### Definition

The **security threshold** is the honest network size at which:
1. The gig economy Sybil attack is economically irrational (cost exceeds expected revenue) at any realistic token price, AND
2. The attacker's fleet cannot reach a meaningful percentage of total nodes without detection

### Threshold Calculation

For the attack to be **meaningfully dangerous** (not just profitable, but capable of influencing consensus), the attacker needs:
- **>5% of total nodes** to influence validator committee selection probability
- **>10% of total nodes** to have non-negligible committee capture risk
- **>33% of total nodes** to stall the network (block 8+ of 21 validators)

#### Scenario A: Attacker targets 10% of network

| Honest Miners | Attacker Fleet Needed | Annual Cost | Breakeven Price | Required Market Cap |
|---------------|----------------------|-------------|-----------------|-------------------|
| 10,000 | 1,111 | $564,000 | $0.0027 | $5.7M |
| 50,000 | 5,556 | $2,817,000 | $0.013 | $28M |
| 100,000 | 11,111 | $5,634,000 | $0.027 | $57M |
| 500,000 | 55,556 | $28,170,000 | $0.133 | $283M |
| 1,000,000 | 111,111 | $56,333,000 | $0.265 | $565M |

#### Scenario B: Attacker targets 5% of network

| Honest Miners | Attacker Fleet Needed | Annual Cost | Breakeven Price | Required Market Cap |
|---------------|----------------------|-------------|-----------------|-------------------|
| 10,000 | 526 | $267,000 | $0.0025 | $5.3M |
| 50,000 | 2,632 | $1,334,000 | $0.013 | $27M |
| 100,000 | 5,263 | $2,668,000 | $0.025 | $54M |
| 500,000 | 26,316 | $13,338,000 | $0.126 | $267M |
| 1,000,000 | 52,632 | $26,675,000 | $0.251 | $535M |

### Detection Multiplier

The cost model above assumes the attacker **is never caught.** But Gratia has progressive slashing:

| Detection Rate | Effective Cost Multiplier | Notes |
|----------------|--------------------------|-------|
| 0% (never caught) | 1.0× | Best case for attacker |
| 5% of fleet caught per month | ~1.6× | Replaces phones + lost stake + carrier downtime |
| 10% of fleet caught per month | ~2.5× | Significant churn; logistics become chaotic |
| 20% of fleet caught per month | ~5.0× | Unsustainable; fleet shrinks faster than it grows |

With TEE attestation, cross-day behavioral analysis, and Bluetooth peer graph detection, a realistic detection rate for a large fleet is **5-15% per month.** This multiplies the attacker's effective annual cost by 1.6-3×.

### Adjusted Security Thresholds (with 10% monthly detection)

| Honest Miners | Attacker Target | Adjusted Annual Cost | Adjusted Breakeven |
|---------------|----------------|---------------------|-------------------|
| 100,000 | 10% (11K phones) | $14,085,000 | $0.066 |
| 100,000 | 5% (5.3K phones) | $6,670,000 | $0.063 |
| 500,000 | 10% (56K phones) | $70,425,000 | $0.332 |
| 500,000 | 5% (26K phones) | $33,345,000 | $0.314 |

---

## Consensus Impact Analysis

Even if an attacker breaks even on mining revenue, can they actually damage the network?

### Validator Committee Capture Probability

With a 21-validator committee selected via VRF weighted by Presence Score:

| Attacker % of Network | P(≥8 of 21 seats) | P(≥14 of 21 seats) | Impact |
|-----------------------|-------------------|--------------------|----|
| 1% | ~0% | ~0% | None |
| 5% | ~0.001% | ~0% | Negligible |
| 10% | ~0.1% | ~0% | Very low |
| 20% | ~5% | ~0.001% | Low (stall possible, finality safe) |
| 33% | ~28% | ~0.1% | Medium (frequent stalls) |
| 50% | ~67% | ~13% | Critical (finality at risk) |

<!-- WHY: These probabilities use the hypergeometric distribution for sampling without replacement
     from a pool of honest and attacker nodes. The 21-validator committee provides strong statistical
     protection even at 20% attacker penetration. -->

### Key Finding

An attacker needs **>33% of the network** to reliably disrupt consensus. At that level:

| Honest Miners | Attacker Fleet (33%) | Annual Cost (with detection) | Breakeven Price |
|---------------|---------------------|------------------------------|-----------------|
| 100,000 | 50,000 | $63,375,000 | $0.298 |
| 500,000 | 250,000 | $316,875,000 | $1.49 |

**To actually break consensus at 500K honest nodes costs ~$317M/year and requires a token price above $1.49 to break even.** A token at $1.49 with 500K nodes implies a market cap of ~$3.2B, making this a nation-state-level attack against a significant financial network.

---

## Security Threshold: Final Determination

### The Number: 100,000 honest miners

At 100,000 honest miners:

| Property | Value |
|----------|-------|
| Cost to maintain 10% penetration | ~$14M/year (with detection) |
| Cost to maintain 33% (consensus threat) | ~$63M/year (with detection) |
| Breakeven token price for 10% attack | $0.066 |
| Breakeven token price for 33% attack | $0.298 |
| Committee capture probability at 10% | ~0.1% per round |
| Committee capture probability at 33% | ~28% per round (stalls, not finality) |
| Finality compromise probability at 10% | ~0% |

**Why 100K is the threshold:**

1. **Economics become unfavorable.** Even a profitable mining attack ($14M/year for 10%) doesn't grant meaningful consensus power. The attacker is just an expensive miner earning the same flat rate as everyone else.

2. **Consensus attack is prohibitively expensive.** Reaching 33% costs $63M+/year, requires managing 50,000 carrier relationships, and the token must be worth $0.30+ for breakeven — implying a market cap where the network has serious defensive resources.

3. **Detection compounds over time.** At 10% monthly detection, the attacker loses ~5,000 phones/month and must constantly recruit and replace carriers. This is a logistics nightmare at scale.

4. **Social pressure activates.** In a 100K+ network, geographic shard analysis, behavioral clustering, and community-level scrutiny make large-scale operations visible.

### Below 100K: The Vulnerability Window

| Phase | Est. Network Size | Risk Level | Mitigation |
|-------|------------------|------------|------------|
| Launch week | 100-1,000 | **Critical** | Graduated committee (smaller committee = harder to capture with VRF) |
| Month 1-3 | 1,000-10,000 | **High** | Founding team runs significant node count; heightened behavioral monitoring |
| Month 3-12 | 10,000-50,000 | **Medium** | Detection systems operational; attack cost rising but still feasible |
| Month 12+ | 50,000-100,000 | **Low-Medium** | Approaching threshold; most attack scenarios unprofitable |
| Mature | 100,000+ | **Low** | Security threshold reached |

### Early Network Defense Recommendations

The first 12 months require specific protections beyond the standard three-wall model:

1. **Graduated committee system** — Smaller committees are statistically harder to capture at low node counts. See separate spec (committee-scaling.md).

2. **Founding node density** — The founding team should operate enough real nodes to maintain >50% of the network during the first 3-6 months. This is not centralization — these are real phones under the same rules — it's bootstrapping security.

3. **Enhanced behavioral monitoring** — During the vulnerability window, PoL behavioral thresholds should be stricter, with more frequent challenges and faster escalation to slashing.

4. **Stake premium for early network** — The 14-day peg already handles this naturally: at 1,000 miners, minimum stake is ~81,508 GRAT per phone. An attacker running 500 phones needs 40,754,000 GRAT in locked stake — a massive capital commitment when the token has negligible value.

5. **Transparent security reporting** — Publish monthly network health reports showing node distribution, behavioral anomaly rates, and geographic diversity. Community vigilance is a force multiplier.

---

## Comparison to Other Networks

| Network | Attack Model | Cost to Attack | Security Basis |
|---------|-------------|----------------|---------------|
| Bitcoin | 51% hashrate | ~$10B+ in ASICs + electricity | Hardware + energy |
| Ethereum | 33% stake | ~$40B+ in ETH | Capital at risk |
| Solana | 33% stake | ~$25B+ in SOL | Capital at risk |
| **Gratia (100K nodes)** | **33% Sybil fleet** | **~$63M/year + detection risk** | **Humans + capital + energy** |
| **Gratia (1M nodes)** | **33% Sybil fleet** | **~$317M/year + detection risk** | **Humans + capital + energy** |

Gratia's security cost is lower in absolute terms but has a fundamentally different character: it requires **sustained human logistics** rather than a one-time capital purchase. You can buy $10B in ASICs once. You cannot buy 333,000 reliable phone carriers once — you must manage that operation every single day, replace detected phones every month, and scale carrier recruitment faster than the honest network grows. The logistics problem doesn't scale.

---

## Open Questions for Further Modeling

- [ ] Year 2-5 emission reduction impact on attacker revenue (25% annual reduction makes attack less profitable over time)
- [ ] Geographic equity multiplier exploitation — could an attacker concentrate in elevated-reward regions?
- [ ] Stake liquidation risk — if attacker is detected mid-year, what fraction of locked stake is recoverable vs. slashed?
- [ ] Second-order effects: does a profitable mining Sybil (that doesn't attack consensus) actually harm the network, or is it just an expensive way to mine?
