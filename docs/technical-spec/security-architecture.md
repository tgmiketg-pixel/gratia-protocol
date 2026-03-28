# Gratia Security Architecture

This document describes how Gratia's security model works — the three-wall defense system, the phone hardware security model, and how mobile-specific challenges are addressed.

---

## Three-Wall Defense System

All three walls must be satisfied simultaneously for a node to participate in consensus. An attacker must defeat all three at once, which is the core security property.

### Wall 1: Proof of Life (Stops Fake Phones)

Proves a real human used a real phone all day. A room full of phones sitting on a shelf fails this. An emulator on a computer fails this. A script tapping a screen in a pattern fails this.

**Required daily parameters (rolling 24-hour window):**
1. Minimum 10 unlock events spread across at least a 6-hour window
2. Screen interaction events showing organic touch patterns at multiple points throughout the day (timing/frequency only, never content)
3. At least one orientation change (phone picked up or moved)
4. Accelerometer data showing human-consistent motion during at least a portion of the day
5. At least one GPS fix confirming a plausible geographic location
6. Connection to at least one Wi-Fi network OR detection of Bluetooth peers
7. Varying Bluetooth peer environments at some point during the day (different device sets at different times)
8. At least one charge cycle event (plug-in or unplug) during the 24-hour period

**Privacy guarantee:** All sensor data is processed on-device. Raw data never leaves the phone. Zero-knowledge proofs attest to parameter completion without revealing the underlying data.

### Wall 2: Staking (Stops Small-Scale Multi-Device Gaming)

Requires locking GRAT tokens to participate in mining. The per-node stake cap prevents wealth from translating into outsized consensus power.

- Minimum stake required (governance-adjustable)
- Per-node stake cap (e.g., 1,000 GRAT)
- Overflow goes to Network Security Pool, benefiting all miners
- Consensus power is identical for all nodes above minimum stake

### Wall 3: Energy Expenditure (Stops Emulators and VMs)

Mining requires real ARM computation, burning real electricity on real phone hardware.

- Exploits ARM big.LITTLE architecture — computation scheduled across efficiency and performance cores in patterns unique to real ARM SoCs
- Power draw and thermal signatures must be consistent with real phone hardware
- x86 emulators cannot efficiently replicate ARM-specific execution patterns
- Even ARM cloud servers lack phone-specific sensor hardware

---

## Phone Hardware Security Model

### Why Phones Are More Secure Than You Think

Modern smartphones (2018+, $50+) have hardware security features that most desktop computers lack entirely.

#### Secure Enclave / TEE (Trusted Execution Environment)

- **Android:** Keystore backed by StrongBox (dedicated security chip) or TEE (ARM TrustZone)
- **iOS:** Secure Enclave (dedicated security coprocessor)
- **Key property:** Private keys are generated inside the secure enclave and NEVER leave it. Not extractable by malware, a rooted OS, or physical device access.
- The main processor sends data to the enclave and asks it to sign. The enclave returns the signature. The key material never crosses the boundary.

<!-- WHY: This is the same security model used by Apple Pay, Google Pay, and banking apps that
     collectively handle billions of dollars in transactions daily. It is proven in production. -->

#### Biometric Authorization

- Every transaction requires biometric authentication (fingerprint or face)
- Biometric templates are stored in the secure enclave, not in software
- Even if the OS is compromised, the attacker cannot authorize transactions without the user's biometric

#### Proof of Life Behavioral Binding

- The wallet is bound not just to a key, but to a behavioral signature
- If a different human begins operating the device (different touch patterns, different movement patterns, different daily rhythms), the protocol detects this
- This is the third layer of wallet security, on top of secure enclave keys and biometrics

### Comparison: Phone Security vs. Desktop Security

| Property | Typical Desktop | Modern Phone |
|----------|----------------|-------------|
| Hardware key storage | No (software only) | Yes (secure enclave) |
| Biometric authorization | Rare | Standard |
| Behavioral binding | No | Yes (via Proof of Life) |
| App sandboxing | Limited | Strict (per-app isolation) |
| Verified boot chain | Rare | Standard |
| Remote wipe capability | Varies | Standard |

---

## Network Security Model

### Why Distribution Beats Raw Power

```
Bitcoin:    ~10,000 mining nodes,  each massively powerful
Gratia:     1,000,000+ nodes,     each modestly powerful
```

Security comes from distribution and redundancy, not individual node power. To attack Bitcoin, you need to outcompute 10,000 warehouses — hard, but it's a single dimension (compute). To attack Gratia, you need to compromise a million phones scattered across every country on earth, each one strapped to a verified human. That's a logistics problem, and logistics problems don't scale.

### Consensus Resilience

- **21-validator committee** selected via VRF each round
- **14/21 (67%) required for finality** — 7 validators can be offline simultaneously
- **Committee rotation** prevents sustained capture
- **3-5 second block time** — generous enough to tolerate mobile network latency

### Mobile Network Challenges and Mitigations

#### Connection Instability

**Problem:** Phones drop connections, switch between Wi-Fi and cellular, go through tunnels.

**Mitigations:**
- **QUIC transport** handles connection migration natively — switching from Wi-Fi to cellular mid-block doesn't drop the connection
- **Gossipsub propagation** — messages spread through multiple paths. If one peer connection drops, the message arrives via another peer
- **3-5 second block time** — deliberately chosen to tolerate mobile latency spikes

#### Intermittent Availability

**Problem:** Phones are not online 24/7. Users unplug, run out of battery, go to airplane mode.

**Mitigations:**
- Mining only requires being plugged in + above 80% — typically 6-10 hours overnight
- The network is designed for millions of nodes where hundreds of thousands are always online
- No single phone is critical — the network tolerates any individual node going offline at any time
- Proof of Life data is collected passively during normal phone use and doesn't require continuous connectivity

#### Storage Constraints

**Problem:** Phones have limited storage compared to servers (typically 32-256 GB, shared with user's photos, apps, etc.).

**Mitigations:**
- **Pruned state:** Phones store only current state (who owns what) plus recent blocks. Target: 2-5 GB maximum.
- **Archive nodes:** Servers can store full history but CANNOT participate in consensus. History is preserved and verifiable without burdening phones.
- **Geographic sharding:** Each phone validates only transactions in its shard, not the entire world's transactions.
- **RocksDB tuning:** Storage engine is specifically tuned for mobile NAND flash — optimized write patterns, controlled compaction scheduling, memory-mapped I/O limits.

---

## Wallet Recovery Security

### Recovery via Proof of Life Behavioral Matching

When a user gets a new phone:

1. User installs Gratia on new device and initiates wallet recovery
2. Old wallet is **immediately frozen** — no transactions in or out
3. New device begins collecting Proof of Life behavioral data
4. Over a 7-14 day window, the protocol compares behavioral patterns between old device history and new device
5. If behavioral patterns match (same human, different device), wallet transfers to new device
6. Old device owner can **reject the claim instantly** from the original device at any time during the recovery window

### Why Not Social Recovery?

<!-- WHY: Social recovery (where trusted friends can collectively restore your wallet) was
     considered and rejected due to collusion vulnerability. In a one-phone-one-vote system,
     social recovery creates a vector where a group of malicious actors could claim someone
     else's wallet by colluding as "trusted contacts." Behavioral matching is strictly
     individual and cannot be colluded. -->

### Optional Backup Methods

- **Seed phrase:** Available in settings, opt-in only, not shown during onboarding, not the default
- **Inheritance:** Designate a beneficiary wallet with a 365-day dead-man switch, opt-in only

---

## Privacy Architecture

### On-Device Processing

All sensor data is processed locally. The protocol never sees raw sensor readings.

```
Phone sensors → Local processing → Zero-knowledge proof → Network

                  ↑ Raw data stays here
                                          ↑ Only the proof leaves the device
```

### Zero-Knowledge Attestation Properties

- **Completeness:** If the user genuinely met all PoL parameters, the proof will verify
- **Soundness:** If the user didn't meet parameters, they cannot create a valid proof
- **Zero-knowledge:** The proof reveals NOTHING about the underlying sensor data — not location, not timing, not behavior patterns
- **Unlinkable:** Attestations between different days cannot be linked to each other — an observer cannot build a behavioral profile over time

### User-Controlled Privacy

| Data Type | Default | User Control |
|-----------|---------|-------------|
| Location granularity | City-level for shard assignment | Adjustable (country → neighborhood) |
| Transaction amounts | Transparent (standard tx) | Can choose shielded tx per transaction |
| Camera / microphone | OFF | Strictly opt-in |
| Sensor data sharing | Never leaves device | Cannot be changed — hardcoded privacy |

---

## Attack Surface Comparison

| Attack Vector | Bitcoin | Gratia |
|--------------|---------|--------|
| 51% compute attack | Need ~$10B in ASICs + electricity | Not applicable — compute doesn't determine consensus |
| Sybil attack (fake identities) | Easy — just spin up nodes | Hard — each node needs a real human, real phone, real life |
| Hardware compromise | ASICs are simple, hard to tamper | Phones have secure enclaves, biometrics, attestation APIs |
| Geographic concentration | ~50% of mining in 2-3 countries | Distributed across every country with smartphones |
| Shutdown risk | Seize a few mining facilities | Seize a million phones from a million people in 100 countries |
| Single point of failure | A few mining pools control majority hashrate | No pools. Each phone is independent. |
| Wealth = power | Yes. More money = more hashrate = more control | Capped. One phone, one vote. Stake is capped. |
| Energy waste | Enormous (~150 TWh/year) | Negligible (~3W incremental per phone, only when already charging) |

---

## Progressive Trust Model

Mining begins immediately upon installation — no onboarding delay. The privilege to mine is granted instantly and maintained through ongoing honest participation. Trust builds progressively in the background while the user earns from day one.

### Trust Tiers

| Day | Trust Level | Mining | Committee Eligible | Governance Eligible | Behavioral Scrutiny |
|-----|------------|--------|--------------------|--------------------|--------------------|
| 0 | Unverified | Yes — full flat rate | No | No | Maximum |
| 1-7 | Provisional | Full rate | No | No | High |
| 7-30 | Establishing | Full rate | No | No | Standard |
| 30-90 | Established | Full rate | Yes | No | Normal |
| 90+ | Trusted | Full rate | Yes | Yes | Standard |

### Design Principles

<!-- WHY: Instant mining exploits loss aversion — taking away a privilege someone already
     has is more motivating than making them wait to earn one. Users who see GRAT
     accumulating on night one will do whatever it takes to keep earning. -->

- **Rewards are identical at every tier.** A Day 0 node earns the same per-minute rate as a Day 90 node. What changes is trust level, scrutiny intensity, and eligibility for network responsibilities.
- **Scrutiny decreases with time.** New nodes face aggressive PoL parameter checking, more frequent verification challenges, and tighter behavioral thresholds. As the node builds history, scrutiny relaxes to normal levels.
- **Committee and governance eligibility are earned.** Only established nodes (30+ days) can be selected for validator committees. Only trusted nodes (90+ days) can submit governance proposals or vote. This prevents an attacker from flooding the network with fresh nodes to influence consensus or governance.
- **Trust resets on slashing.** A node that receives a Major or Critical slash drops back to Unverified, regardless of how long it has been mining. Trust must be re-earned.
- **The user never sees the tiers.** There is no UI showing "you are Provisional." The user just mines and earns. Trust is an internal protocol concept, not a user-facing feature.

### Security Implications

The progressive trust model interacts with the other security layers:

- **Graduated committee scaling** uses trust tiers as a selection filter — only Established+ nodes enter the VRF selection pool for validator committees
- **Progressive slashing** is more aggressive for lower trust tiers — an Unverified node flagged for anomalies may be slashed faster than a Trusted node with the same anomaly
- **Cross-day behavioral analysis** requires 30+ days of data to produce a reliable fingerprint — this naturally aligns with the Establishing → Established transition
- **Mutual staking (Phase 2)** requires Trusted status to vouch for new nodes, creating an organic onboarding pipeline

---

## Proof of Life Hardening

Three strengthening measures that layer on top of the base PoL parameter system to close remaining attack vectors.

### TEE Attestation as Primary Signal

Currently TEE attestation adds +8 to the Composite Presence Score and is not required for the consensus threshold. This should be strengthened.

<!-- WHY: The current +8 bonus treats TEE as a nice-to-have. In practice, TEE attestation is
     the single strongest anti-spoofing signal available — it cryptographically proves hardware
     authenticity in a way that no behavioral heuristic can match. Elevating it to primary
     status closes the largest remaining gap in PoL without making it a hard gate. -->

- **Android SafetyNet / Play Integrity** and **iOS DeviceCheck** can cryptographically prove the device is real, not rooted, and running a genuine OS
- TEE attestation should be weighted as a **primary PoL signal**, not a bonus — it is the strongest single indicator of hardware authenticity
- Nodes **without** TEE attestation should face heightened behavioral scrutiny:
  - More frequent PoL verification challenges (e.g., 2x daily instead of 1x)
  - Stricter behavioral thresholds across all other PoL parameters
  - Lower tolerance for marginal or borderline parameter readings
- TEE attestation must remain **optional** (not hard-required) to support custom ROMs, phones without Google services, and older devices that lack StrongBox — but passing without it should be significantly harder

**Attack vectors blocked:**
- Robotic phone farms (no TEE — ARM server boards and dev boards lack StrongBox/Secure Enclave)
- Rooted phone sensor spoofing (TEE detects root/bootloader unlock)
- ARM server emulators (no StrongBox/Secure Enclave hardware)

### Cross-Day Behavioral Anomaly Detection

Current PoL validates each day independently. This creates a blind spot: an attacker who can pass a single day's PoL check can replay the same pattern indefinitely. Cross-day consistency analysis closes this gap.

<!-- WHY: Single-day validation was a deliberate simplification for Phase 1. But real humans
     have consistent-but-varying behavioral signatures over time. Checking cross-day consistency
     is the natural next step — it catches replay attacks and device-sharing schemes that
     single-day checks cannot detect. -->

- Compare behavioral patterns across **30+ day rolling windows**
- A real human has consistent but naturally varying patterns:
  - Similar unlock times (plus or minus 1-2 hours day to day)
  - Similar movement ranges and daily distances
  - Similar charge cycle timing
  - Gradual behavioral evolution over months (new habits, schedule changes)
- **Red flags:**
  - Identical patterns day after day → replay attack (too similar)
  - Wildly inconsistent patterns → different people using the same device (behavioral discontinuities)
  - No behavioral evolution over time → bot or scripted input (static)
- **Implementation:** Rolling 30-day behavioral fingerprint computed on-device, compared against the node's own historical baseline
- **Privacy guarantee:** The behavioral fingerprint is computed entirely on-device and attested via ZK proof — the network sees `behavioral_consistency_score: 87/100`, never the underlying data

**Attack vectors caught:**
- Replay attacks (cross-day similarity exceeds natural human variance)
- Sensor injection (inconsistent behavioral signature over time)
- Phone sharing schemes (behavioral discontinuities between different operators)

### Bluetooth Peer Graph Analysis

Network-level defense that analyzes Bluetooth peer diversity across nodes. This operates at the network layer, not the device layer.

<!-- WHY: Per-device checks can only validate one phone at a time. Phone farms are a
     multi-device problem — the defining characteristic is co-location. Bluetooth peer graph
     analysis exploits co-location directly: phones in the same room see the same Bluetooth
     peers. This is the only defense that scales with the attack itself. -->

- If 50 phones always see the same Bluetooth peers at the same times, they are almost certainly co-located in a farm
- Each node reports (via ZK proof) a **hash of their daily Bluetooth peer set** — not the actual device IDs, not the MAC addresses
- The network compares peer set hashes across nodes:
  - Legitimate users in different locations see different peer sets
  - Co-located farm phones see identical or near-identical peer sets
- **Cluster detection:** If N nodes consistently share >80% of their peer set hashes over 14+ days, flag the cluster for enhanced PoL verification
- This is a **network-wide defense**, not a per-device check — requires meaningful node count to be statistically effective (Phase 2+)

**Attack vectors blocked:**
- Physical phone farms of any size (co-location creates identical peer graphs)
- Human-assisted farming with co-located carriers (same room, same peers)

---

## Staking as Security Amplifier

Core design constraint: staking must increase security **without** giving wealth disproportionate power. Stake determines how much you can **lose**, never how much you can **earn**. Mining rewards stay flat per minute for every node regardless of stake.

<!-- WHY: Most PoS systems conflate "security deposit" with "earning multiplier," which
     inevitably concentrates power in wealthy hands. Gratia separates these concerns entirely.
     Stake is purely a cost-of-attack multiplier — it makes cheating expensive without making
     honesty more profitable for the rich. -->

### Three-Pillar Mental Model

- **Proof of Life** = the lock on the door (stops most attackers)
- **Staking** = the alarm system (makes getting caught expensive)
- **Energy** = the security camera (proves real work happened)

### Phase 1 Mechanisms (Launch)

#### 1. Flat Bond

One fixed amount per phone. No range, no tiers. This is a security deposit, not an investment.

<!-- WHY: A flat bond means phone farms pay capital x number_of_phones. A whale with one phone
     pays the same as anyone else. The amount is pegged to mining output so it auto-adjusts:
     cheap when the network is young (low barrier to entry), meaningful when the network is
     mature (strong deterrent). -->

- Phone farms pay `bond_amount x number_of_phones` — linear capital cost that scales with attack surface
- Bond amount pegged to approximately **14 days of average mining rewards** (self-adjusting)
- When the network is young and rewards are low, the bond is small — low barrier to entry
- When the network is mature and rewards are meaningful, the bond is proportionally larger — strong deterrent
- Always "two weeks of your own output" — intuitive and fair at any network size

#### 2. Progressive Slashing

Escalating punishment based on offense history. Honest users who have a bad sensor day are not destroyed. Attackers face compounding cost.

<!-- WHY: Binary slash-or-not systems either slash too harshly (destroying honest users with
     flaky hardware) or too leniently (making fraud cheap). Progressive slashing matches
     punishment to intent: mistakes get warnings, patterns get penalties, proven fraud gets
     permanent removal. -->

| Offense | Consequence |
|---------|------------|
| 1st offense | Warning + 48-hour mining pause |
| 2nd offense within 90 days | 10% stake slashed |
| 3rd offense within 90 days | 50% slashed + 30-day lockout |
| Proven fraud (any time) | 100% burned permanently |

- The 90-day window resets — a single offense 6 months ago does not count against you today
- "Proven fraud" is distinct from "failed PoL check" — it requires validator committee confirmation of deliberate manipulation
- Design principle: the cost of repeated cheating grows faster than the potential reward

#### 3. Fraud Reporter Share

When a node is slashed for proven fraud, the slashed stake is distributed to create active incentives for fraud detection.

- **70%** of slashed stake is **burned** (deflationary pressure)
- **30%** split among the validator committee members that confirmed the fraud
- **Cap:** No single reporter can earn more than their own stake from one report

<!-- WHY: The reporter cap prevents a perverse incentive where nodes with large stakes
     false-flag small nodes for profit. If your maximum reward from reporting is capped at
     your own stake, the incentive to fabricate fraud reports is bounded. -->

- Creates active incentive to detect and report fraud — validators are rewarded for vigilance
- The 70/30 burn-to-reward split ensures fraud is always net-deflationary for the network

### Phase 2 Mechanisms (Requires Network Scale)

#### 4. Mutual Staking (Peer Bonds)

Nodes with 90+ days of unbroken PoL history can co-sign newer nodes by locking a small additional bond. If the vouched-for node is proven fraudulent, **both** stakes are slashed.

<!-- WHY: This creates an organic social trust layer. Real humans vouch for people they know.
     Phone farms cannot get legitimate vouchers because no honest node would risk their own
     stake for a stranger operating 50 phones in a warehouse. -->

- Creates organic social trust layer — real humans vouch for people they actually know
- Phone farms cannot obtain legitimate vouchers (no honest node would risk their stake for an unknown farm operator)
- Vouching is strictly optional — nodes can participate fully without it

#### 5. Geographic Stake Pooling

If a cluster of phones in the same cell tower range are proven fraudulent (phone farm detected), every node in that geographic shard faces 30 days of heightened PoL scrutiny.

- **Not slashing** — just increased verification frequency during the scrutiny period
- Creates social pressure against local farms without punishing innocent neighbors financially
- Legitimate nodes in the affected area continue mining normally, just with more frequent PoL checks

<!-- WHY: This is a soft deterrent, not a punishment. The goal is to make phone farm operators
     unwelcome in their geographic neighborhood by imposing a minor inconvenience on nearby
     honest nodes — who then have social incentive to report suspicious activity. -->

#### 6. Uptime Stake Decay

Required stake decreases with continuous honest participation, rewarding loyalty without granting more mining rewards or governance votes.

| Continuous PoL Duration | Required Stake |
|------------------------|---------------|
| 0-12 months | 100% (full bond) |
| 1 year unbroken PoL | 50% of bond |
| 2+ years unbroken PoL | 25% of bond |

- Rewards long-term honest participation without giving more mining rewards or votes
- Fresh nodes and new attack nodes always pay full price
- Any slashing event resets the decay timer to zero

### What NOT to Do

These anti-patterns must be avoided to preserve the one-phone-one-vote principle:

- **Do not make rewards proportional to stake** — that is just Proof of Stake with extra steps
- **Do not let stake override PoL failure** — a node that fails Proof of Life cannot mine, regardless of how much is staked
- **Do not raise the per-node cap so high that whales dominate validator selection** — the cap exists to bound the influence of wealth

### Summary Table

| Mechanism | Hurts | Helps |
|-----------|-------|-------|
| Flat bond | Phone farms (capital x phones) | Everyone equally |
| Progressive slashing | Repeat offenders | Honest users who make mistakes |
| Fraud reporter share | Fraudsters | Attentive validators |
| Mutual staking | Isolated/anonymous farms | Community participants |
| Geographic pooling | Concentrated farms | Distributed network |
| Uptime decay | New/fresh attack nodes | Long-term participants |

---

## Security Development Roadmap

### Phase 1 (PoC)
- Implement secure enclave key storage via Android Keystore
- Basic Proof of Life parameter validation
- Bulletproofs for PoL attestations
- Ed25519 transaction signing

### Phase 2 (Testnet)
- Full three-wall validation
- VRF-based committee selection
- Behavioral analysis for Proof of Life
- TEE attestation elevated to primary PoL signal
- Cross-day behavioral anomaly detection (30-day rolling fingerprint)
- Progressive slashing implementation
- Fraud reporter share mechanism
- Slashing conditions for malicious validators
- Adversarial red-team testing
- Bluetooth peer graph analysis (late Phase 2 — requires meaningful node count)

### Phase 3 (Mainnet)
- Professional security audit of Rust core, ZK proofs, and consensus
- Graduated committee system for early network protection
- Behavioral clustering detection for Sybil resistance
- Geographic shard security model
- Published threat model for community review
