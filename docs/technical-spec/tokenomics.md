# Gratia Tokenomics Specification

## Token Overview

| Property | Value |
|----------|-------|
| Ticker | GRAT (working name) |
| Smallest unit | 1 Lux (1 GRAT = 1,000,000 Lux) — working name |
| Maximum supply | 10,000,000,000 GRAT (10 billion) |
| Classification | Commodity (not a security) |

### Why 10 Billion Max Supply

- With 1 billion eventual users, that's 10 GRAT per person on average — people hold whole coins, not scary decimals
- Large enough to feel like money in your hand, not a fraction
- Small enough to not feel worthless like a 100-trillion-supply meme coin
- Clean, round, easy to reason about
- For comparison: Bitcoin 21M, Ethereum ~120M, Cardano 45B, XRP 100B

---

## Supply Allocation

```
Mining emission:     8,500,000,000 GRAT  (85%)
Founding allocation: 1,500,000,000 GRAT  (15%)
```

### Founding Allocation Breakdown

| Bucket | Amount | % of Total | Vesting |
|--------|--------|------------|---------|
| Development fund | 600,000,000 | 6% | 4-year linear vest |
| Core team | 400,000,000 | 4% | 1-year lock, then 3-year linear vest |
| Ecosystem grants | 500,000,000 | 5% | Distributed over 5 years |
| **Total founding** | **1,500,000,000** | **15%** | |

The founding 1.5B is minted at genesis but locked in on-chain vesting contracts. The founding team mines under the same rules as everyone else on top of this. Nothing is liquid at launch.

### Fair Launch Principles

- Genesis block mined by founding team on real phones under same rules as everyone
- NO private investor pre-sale at a discount
- 85% emitted through mining only
- No tokens are liquid at launch — all founding allocation is vested

---

## Emission Schedule

Mining emission follows a 25% annual reduction. This is gentler than Bitcoin's 50% halving, providing a smoother transition for miners and the ecosystem.

### Year 1 Emission Derivation

```
Total mining supply = Y1_emission / (1 - 0.75)
8,500,000,000 = Y1 × 4
Y1 = 2,125,000,000 GRAT
```

### Emission Table

| Year | Annual Emission | Daily Budget | Cumulative Mined | % of Mining Supply |
|------|----------------|-------------|------------------|-------------------|
| 1 | 2,125,000,000 | 5,822,000 | 2.13B | 25.0% |
| 2 | 1,593,750,000 | 4,366,000 | 3.72B | 43.8% |
| 3 | 1,195,312,500 | 3,275,000 | 4.91B | 57.8% |
| 4 | 896,484,375 | 2,456,000 | 5.81B | 68.3% |
| 5 | 672,363,281 | 1,842,000 | 6.48B | 76.3% |
| 10 | 150,998,592 | 413,700 | 7.90B | 92.9% |
| 15 | 33,931,398 | 92,962 | 8.36B | 98.4% |
| 20 | 7,625,551 | 20,891 | 8.46B | 99.6% |

These figures are before burns. With transaction fees and poll costs being burned, actual circulating supply will be lower.

---

## Reward Distribution Mechanism

The daily emission budget is fixed per the schedule above. It is divided equally among all active miners proportional to their mining minutes that day.

### How It Works

1. Each day, the protocol has a fixed GRAT budget to distribute (daily budget from table above)
2. Every miner who is active (plugged in, above 80% battery, valid Proof of Life, minimum stake) earns at the same per-minute rate
3. The per-minute rate = daily_budget / total_mining_minutes_across_all_miners
4. Within any given day, every miner earns the same rate per minute — flat, no diminishing returns

### Early Adopter Incentive

Because the daily budget is fixed and split among miners, fewer miners = more per miner. This creates a natural early adopter incentive:

| Active Miners | GRAT Per 8hr Night | GRAT Per Minute |
|--------------|-------------------|-----------------|
| 1,000 (launch week) | 5,822 | 12.13 |
| 10,000 (month 1-2) | 582 | 1.21 |
| 50,000 (mid-year) | 116 | 0.24 |
| 100,000 (end of Y1) | 58 | 0.12 |
| 500,000 (early Y2) | 8.7 | 0.018 |

<!-- WHY: This mirrors Bitcoin's early mining dynamic where early participants earned disproportionately
     more, creating organic grassroots growth incentives without any marketing spend. -->

---

## Staking Model: Capped with Overflow Pool

### Mechanics

- **Minimum stake:** Required to participate in mining (exact amount TBD, governance-adjustable)
- **Per-node stake cap:** e.g., 1,000 GRAT — governance-adjustable
- **Overflow:** Any stake above the cap flows into the Network Security Pool
- **Pool yield:** Distributed proportionally to ALL active mining nodes

### Examples

| Staker | Amount Staked | Active Stake | Overflow to Pool | Consensus Power |
|--------|-------------|-------------|-----------------|-----------------|
| Alice | 500 GRAT | 500 GRAT | 0 | Standard |
| Bob | 1,000 GRAT | 1,000 GRAT | 0 | Standard |
| Whale | 50,000 GRAT | 1,000 GRAT | 49,000 GRAT | Standard (same as Bob) |

The whale earns yield on their full 50,000 GRAT stake, but their consensus power is capped at the same level as Bob. The 49,000 overflow subsidizes the Network Security Pool, which benefits all active miners proportionally.

### Design Intent

<!-- WHY: Wealth concentration is deliberately channeled into subsidizing small miners rather than
     granting outsized power. This ensures one-phone-one-vote governance cannot be circumvented
     by staking more tokens. -->

### Concrete Staking Parameters

**Minimum Stake (Flat Bond):**

- Pegged to ~14 days of average mining rewards at current network size
- Self-adjusting formula: `min_stake = (daily_budget / active_miners) × 14`
- At launch (1,000 miners): ~14 × 5,822 GRAT/night = ~81,508 GRAT per node
- At 10,000 miners: ~14 × 582 = ~8,148 GRAT per node
- At 100,000 miners: ~14 × 58 = ~812 GRAT per node
- At 1,000,000 miners: ~14 × 5.8 = ~81 GRAT per node
- Design principle: always "2 weeks of your own output" — cheap when network is young (low barrier → growth), meaningful when mature (strong deterrent)
- Recalculated by governance vote quarterly, or when active miner count changes by >25%

<!-- WHY: The 14-day peg means every miner has exactly enough skin in the game that getting caught
     cheating costs them 2 weeks of earnings. This is the minimum meaningful deterrent — short
     enough that new users can accumulate stake quickly from mining, long enough that losing it
     stings. -->

**Per-Node Stake Cap:**

- Set at 100× the minimum stake
- At launch: ~8,150,800 GRAT cap (effectively very high — irrelevant early)
- At 1M miners: ~8,100 GRAT cap
- Everything above cap flows to Network Security Pool as already designed
- Cap scales with minimum stake automatically

---

## Progressive Slashing Schedule

| Offense | Penalty | Stake Impact | Mining Impact |
|---------|---------|-------------|---------------|
| 1st offense | Warning | None | 48-hour mining pause |
| 2nd within 90 days | Minor slash | 10% of effective stake burned | Mining resumes after slash |
| 3rd within 90 days | Major slash | 50% of effective stake burned | 30-day mining lockout |
| Proven fraud (any time) | Full slash | 100% burned permanently | Permanent ban |

<!-- WHY: Progressive slashing protects honest users from catastrophic loss due to sensor glitches
     or edge cases, while making sustained cheating economically devastating. The 90-day rolling
     window means a single bad day doesn't haunt you forever, but a pattern of bad days escalates
     rapidly. -->

**Slash Destination:**

- Proven fraud: 70% burned (deflationary), 30% split among validator committee that confirmed it
- All other slashes: 100% burned (deflationary)
- Reporter cap: no single validator earns more than their own stake from one fraud report

**90-Day Rolling Window:**

- Offense count resets after 90 days of clean participation
- A node slashed at Minor that stays clean for 90 days returns to Warning-level next offense
- Proven fraud bypass: fraud evidence can be submitted at any time and always results in full slash regardless of history

---

## Phase 2 Staking Mechanisms

The following mechanisms require network scale to be meaningful and are planned for Phase 2.

**Mutual Staking (Peer Bonds):**

- 90+ day nodes can vouch for newer nodes by locking additional bond
- If vouched node commits fraud, both stakes slashed
- Creates organic trust network; farms can't get vouched
- Requires enough long-term nodes to be meaningful → Phase 2

**Geographic Stake Pooling:**

- Fraudulent cluster in a cell tower range → all nodes in that geographic area face 30 days heightened PoL scrutiny
- Not slashing, just increased verification frequency
- Requires geographic sharding infrastructure → Phase 2

**Uptime Stake Decay:**

- 1 year unbroken honest PoL: required stake drops to 50%
- 2 years: 25%
- Rewards long-term honest participation without affecting mining rewards or governance votes
- Fresh/attack nodes always pay full stake price

<!-- WHY: These mechanisms all depend on having a large enough network that geographic clusters,
     long-term reputation, and peer trust are statistically meaningful. Deploying them at launch
     with 1,000 miners would create noise, not signal. -->

---

## Deflationary Mechanisms (Burns)

| Source | Mechanism |
|--------|-----------|
| Transaction fees | Burned (not paid to validators) |
| Poll creation costs | Burned |
| Smart contract gas | Burned |

### Monetary Policy Trajectory

```
Early years:   Mining emission >>> burn rate    → inflationary (supply growing)
Middle years:  Mining emission ≈ burn rate      → roughly neutral
Late years:    Mining emission <<< burn rate    → deflationary (supply shrinking)
```

As adoption grows, more transactions and contract executions increase burn rate, while emission keeps dropping 25% per year. Eventually burn outpaces emission and circulating supply begins shrinking.

---

## Energy Cost Basis (Intrinsic Floor Value)

Every GRAT has a real energy cost to produce. This sets a natural floor value.

### Mining Energy Cost Per 8-Hour Session

| Component | Power Draw |
|-----------|-----------|
| Phone charging (baseline — user pays anyway) | ~7W |
| Mining CPU load (incremental) | ~3W |
| **Incremental mining cost** | **3W × 8 hours = 24Wh = 0.024 kWh** |

| Region | Electricity Rate | Cost Per 8hr Session |
|--------|-----------------|---------------------|
| Developing world | ~$0.05/kWh | $0.0012 |
| Global average | ~$0.10/kWh | $0.0024 |
| US average | ~$0.16/kWh | $0.0038 |
| Europe (high end) | ~$0.30/kWh | $0.0072 |

### Implied Energy Floor Per GRAT

| Network Size | GRAT Earned/Night | Energy Cost (Global Avg) | Floor Price Per GRAT |
|-------------|-------------------|-------------------------|---------------------|
| 10,000 miners | 582 | $0.0024 | $0.0000041 |
| 100,000 miners | 58 | $0.0024 | $0.0000414 |
| 1,000,000 miners | 5.8 | $0.0024 | $0.000414 |

<!-- WHY: The initial energy floor is fractions of a cent by design. Bitcoin's energy floor in 2009
     was essentially zero. The energy floor rises naturally as more miners join (same daily budget,
     split more ways = each GRAT costs more energy to produce). A low starting cost means anyone
     on earth can afford to mine. The market value will be driven by utility and demand above this
     floor. -->

### Early Market Cap Scenarios (Year 1)

At ~2.13B GRAT in circulation by end of year 1:

| Price Per GRAT | Market Cap |
|---------------|------------|
| $0.0005 (energy floor @ 1M miners) | ~$1,065,000 |
| $0.01 (early demand) | ~$21,300,000 |
| $0.10 (meaningful adoption) | ~$213,000,000 |

---

## Geographic Equity

Underserved regions earn elevated mining rewards. This is implemented as a multiplier on the base per-minute rate for nodes in regions with low network density.

<!-- WHY: The protocol's fairness principle states that every design decision should be tested
     against "Does this benefit a wealthy user more than a poor user?" Geographic equity ensures
     that network growth is incentivized in underserved areas rather than concentrating rewards
     in wealthy countries with high early adoption. -->

---

## Key Adjustable Parameters

The following parameters are governance-adjustable (one-phone-one-vote):

| Parameter | Initial Value | Adjustable Via |
|-----------|--------------|----------------|
| Minimum stake | 14 days avg mining rewards (self-adjusting) | Governance vote |
| Per-node stake cap | 100× minimum stake | Governance vote |
| Emission reduction rate | 25% annually | Governance vote |
| Geographic equity multipliers | TBD | Governance vote |
| Transaction fee burn rate | 100% burned | Governance vote |
| Poll creation cost | TBD | Governance vote |

---

## Open Questions

- [x] Minimum stake: pegged to 14 days average mining rewards (self-adjusting)
- [x] Per-node stake cap: 100× minimum stake
- [x] Slashing schedule: progressive 4-tier system with 90-day rolling window
- [ ] Geographic equity multiplier specifics — which regions, what multiplier, how measured
- [ ] Maximum supply finality — 10B is current recommendation, subject to founder decision
- [ ] Mutual staking bond amount — needs modeling (Phase 2)
- [ ] Geographic pooling radius — needs cell tower density analysis (Phase 2)
