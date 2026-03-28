# Gratia Threat Model

This document catalogs known attack vectors against the Gratia protocol, assesses their severity, and documents current and proposed mitigations. This is a living document — all contributors and security researchers are encouraged to identify additional vectors.

---

## Threat Model Philosophy

Gratia's security rests on three simultaneous walls:

1. **Proof of Life** — stops fake phones and bots
2. **Staking** — stops small-scale multi-device gaming
3. **Energy Expenditure** — stops emulators and virtual machines

An attacker must defeat all three walls simultaneously to compromise the network. The threat model analyzes attacks against each wall individually and in combination.

---

## Threat Severity Ratings

| Rating | Definition |
|--------|-----------|
| **Critical** | Could compromise consensus or steal funds at scale |
| **High** | Could degrade network integrity or enable meaningful reward theft |
| **Medium** | Could affect a subset of users or a single shard |
| **Low** | Theoretical concern with high cost and limited impact |

---

## TIER 1: Attacks We Handle Well

### 1.1 Emulator / Virtual Machine Farms

**Attack:** Run thousands of Android emulators on servers to mine GRAT without real phones.

**Severity:** Low (well-mitigated)

**Why it fails:**
- Wall 3 (Energy Expenditure) requires real ARM silicon. Emulators running on x86 servers cannot replicate ARM big.LITTLE scheduling behavior, real power draw curves, or thermal signatures.
- ARM-specific computation means the work literally cannot execute efficiently on non-ARM hardware.
- Even ARM servers (e.g., AWS Graviton) lack phone-specific sensors (GPS, accelerometer, Bluetooth peers, ambient light).

**Status:** Solved by design.

---

### 1.2 Phone-on-a-Shelf Farms

**Attack:** Buy 100 phones, place them on shelves, run mining software without human interaction.

**Severity:** Low (well-mitigated)

**Why it fails:**
- Wall 1 (Proof of Life) requires within a rolling 24-hour window:
  - 10+ unlock events spread across 6+ hours
  - Organic touch patterns at multiple points throughout the day
  - Orientation changes (phone picked up or moved)
  - Accelerometer data showing human-consistent motion
  - GPS fix showing plausible location
  - Wi-Fi or Bluetooth peer connectivity
  - Varying Bluetooth peer environments at different times
  - At least one charge cycle event
- A phone sitting on a shelf fails unlock patterns, accelerometer motion, GPS variation, Bluetooth peer diversity, and orientation changes.

**Status:** Solved by design.

---

### 1.3 Screen-Tapping Bot Scripts

**Attack:** Use automated scripts or robotic arms to simulate human touch patterns on real phones.

**Severity:** Low-Medium

**Why it mostly fails:**
- Proof of Life behavioral analysis detects:
  - Uniform tap timing (humans are irregular)
  - Identical touch coordinates (humans vary)
  - No variation in pressure or gesture type
  - Absence of natural scrolling, swiping, and multi-touch patterns
  - Statistically regular intervals between interactions

**Residual risk:** Sophisticated scripts with randomized timing and coordinates could potentially pass basic pattern analysis. Advanced behavioral modeling (ML-based anomaly detection) increases the bar significantly.

**Status:** Largely solved. Continuous improvement of behavioral analysis recommended.

---

## TIER 2: Attacks That Are Harder to Stop

### 2.1 Gig Economy Sybil Attack — HIGHEST PRIORITY THREAT

**Attack:** Pay real humans in low-income regions $1/day to carry an attacker-controlled phone and use it normally throughout the day. The person lives their life, passes Proof of Life completely legitimately, and plugs the phone in at night. The attacker controls the wallet keys.

**Severity:** Critical

**Why it's dangerous:** This attack passes ALL THREE WALLS legitimately.
- Real human → passes Proof of Life
- Real phone → passes energy expenditure
- Attacker stakes tokens → passes staking requirement

**Cost analysis:**
- 10,000 phones × $1/day × 365 days = $3.65M/year in labor
- 10,000 budget phones (~$50 each) = $500K hardware
- Electricity, logistics, management = ~$500K/year
- **Total: ~$4.65M/year for 10,000 nodes**

**At what point is it dangerous:**
- At 10,000 attacker nodes, begins influencing validator committee selection
- At 50,000+ attacker nodes, could approach meaningful consensus power in small shards
- Economic rationality depends on network value — attack only makes sense if potential gain exceeds the multi-million dollar annual cost

**Current mitigations:**
- Stake cap limits consensus power per node — even 50,000 nodes can't dominate a network of millions
- Geographic sharding distributes impact — concentrated phones in one country only affect that shard
- Cost vs. reward math makes attack irrational until network is worth hundreds of millions, by which time honest nodes vastly outnumber attackers

**Proposed additional mitigations:**
- **Behavioral clustering detection:** Flag groups of nodes that all plug in at the same time, mine the same hours, or exist in the same geographic region with suspiciously similar patterns
- **Anomaly detection on phone usage diversity:** Gig workers using a dedicated "mining phone" will show different app usage patterns than someone using their personal phone
- **Social graph analysis:** Legitimate users connect to diverse Bluetooth peers. Gig workers in the same facility would see the same peer sets repeatedly

**Open problems:**
- A sufficiently patient, well-funded attacker who staggers behavior across regions could run this for a long time before detection
- This attack fundamentally cannot be prevented — only made expensive and detectable
- **Action item:** Model the gig economy Sybil mathematically. At what network size does it become economically irrational? That number is our "security threshold."

---

### 2.2 Validator Committee Capture

**Attack:** Accumulate many high-Presence-Score nodes. Wait for VRF rounds where the attacker gets disproportionate committee seats. With 8+ of 21 seats (38%+), can stall the network. With 14+ seats (67%+), can finalize arbitrary blocks.

**Severity:** Critical (especially in early network)

**Probability analysis:**
- With 100 attacker nodes in a network of 100,000 (0.1%), probability of getting 14+ seats is astronomically low
- With 5,000 attacker nodes in a network of 50,000 (10%), probability becomes non-negligible
- **Early network is the vulnerability window** — few total nodes means attacker's percentage is higher

**Current mitigations:**
- Committee rotation every block or every few blocks — attacker needs sustained luck
- Repeated stalls from the same nodes trigger slashing
- VRF is weighted by Presence Score, making it harder to game selection

**Proposed additional mitigations:**
- **Graduated committee system:** Scale committee size with network size. At 10,000 total nodes, committee could be 7 validators. At 100,000, scale to 15. At 1M+, full 21-validator committee.
- **Cooldown periods:** Prevent the same node from being selected in consecutive committee rounds
- **Statistical monitoring:** Alert if any entity's committee selection frequency exceeds expected bounds

**Open problems:**
- **Action item:** Model minimum network size where 21-validator VRF selection is statistically safe. Define the graduated committee scaling curve.

---

### 2.3 Geographic Shard Attack

**Attack:** Flood a small geographic shard with attacker-controlled nodes until reaching majority in that shard's validator set. Control consensus for all transactions in that region.

**Severity:** High

**Why it's dangerous:**
- Each shard has fewer nodes than the total network
- Small countries or regions with low adoption could have only a few thousand nodes
- Concentrating 2,000+ attacker nodes in a shard of 5,000 gives near-majority

**Current mitigations:**
- Minimum shard size thresholds — if a shard falls below a node count, merge with neighbor
- Cross-shard validation — other shards periodically audit each other's blocks

**Proposed additional mitigations:**
- **Shard assignment randomization:** Mix geographic and random assignment so attacker can't predict which shard nodes land in
- **Cross-shard validator rotation:** Periodically rotate some validators between shards
- **Shard health monitoring:** Track unusual transaction patterns, validator concentration, or finality delays per shard

**Open problems:**
- Tension between "geographic sharding for performance" and "geographic concentration as attack vector"
- **Action item:** Develop a detailed sharding spec that addresses this tension, including minimum shard sizes and merge/split criteria.

---

### 2.4 SafetyNet / Play Integrity Dependency

**Attack:** Not a direct attack, but a platform risk. If Gratia relies on Google Play Integrity or Apple DeviceCheck to verify real devices, Google/Apple could:
- Change the API and break attestation overnight
- Revoke access for crypto apps
- Be compelled by a government to flag Gratia nodes

**Severity:** Medium (platform risk, not adversarial attack)

**Design decision:**
- Platform attestation APIs should be **one signal among many**, never the sole source of truth
- Gratia's own Proof of Life attestation is the primary wall
- The protocol MUST function correctly on phones that fail Play Integrity (rooted phones, custom ROMs, phones without Google services)

**Tension:** If we don't require Play Integrity, rooted phones can fake sensors more easily. If we do require it, we're dependent on Google/Apple.

**Resolution:** Use it when available as a bonus signal. Compensate with stronger behavioral analysis on phones without it. Never make it a hard requirement.

---

### 2.5 Slow Behavioral Poisoning

**Attack:** Run 1,000+ nodes with subtly artificial behavior patterns — not enough to fail Proof of Life, but enough to shift what the network considers "normal." Over 6-12 months, the definition of organic behavior drifts. Then introduce bots that match the new, corrupted baseline.

**Severity:** High

**Why it's insidious:**
- No single day triggers a flag
- The attack is gradual and statistical
- By the time the drift is noticed, the behavioral baseline is already corrupted
- Corrupted baseline then allows more sophisticated bots to pass

**Current mitigations:**
- Proof of Life parameters are defined as absolute thresholds (10+ unlocks, 6+ hour spread, etc.), not relative to network behavior — this anchors the baseline

**Proposed additional mitigations:**
- **Anchor behavioral baselines to published academic research** on human phone usage patterns, not just network-observed behavior
- **Maintain an independent reference dataset** separate from current network participants
- **Monitor population-level behavioral drift:** Anomaly detection should flag when the network's aggregate behavioral distribution shifts over time, not just individual outliers
- **Periodic behavioral model audits:** Review and update the PoL behavioral model against fresh academic data on phone usage

**Open problems:**
- Who maintains the reference baseline? If the founding team, that's centralization. If governance, an attacker could influence governance to accept a drifted baseline.
- **Action item:** Define the governance model for behavioral baseline updates. Consider a security council or academic partnership for baseline validation.

---

### 2.6 Proof of Life Spoofing Vectors

The following five attack vectors specifically target the Proof of Life system — Gratia's primary wall against phone farms and bots. Each vector attempts to defeat or circumvent PoL through a different approach, ranging from physical manipulation to software-level sensor spoofing.

---

#### 2.6.1 Robotic Phone Farms

**Attack:** Mechanical rigs that physically manipulate real phones — robotic arms pressing screens, motorized platforms creating movement, automated charging cycles.

**Severity:** Medium-High

**Why it's dangerous:**
- Passes Wall 3 (real ARM hardware) completely — the phone is genuine
- Passes Wall 2 (can stake tokens) — attacker controls the wallet
- Wall 1 (Proof of Life behavioral analysis) is the only remaining defense
- Robotic arms can simulate unlock patterns, touch events, and orientation changes
- Motorized platforms generate accelerometer data that could mimic human carrying motion

**Current mitigations:**
- PoL behavioral analysis detects uniform patterns — mechanical rigs produce statistically regular timing between interactions
- Accelerometer expects human-consistent motion, not the mechanical regularity of a motorized platform
- Touch pattern analysis looks for organic variation in pressure, coordinates, and gesture types

**Proposed mitigations:**
- TEE attestation weighted more heavily — currently contributes +8 Presence Score, but consider making it significantly harder to pass PoL without it, since robotic farms cannot fake TEE-bound attestations
- Cross-day behavioral consistency checks — does the same human appear to be operating this phone over 30+ consecutive days? Robots produce subtly different behavioral signatures than humans over long periods
- Touch pressure and gesture variety analysis — humans exhibit a wide distribution of gesture types (scrolls, swipes, pinches, long presses) that mechanical arms struggle to replicate naturally

**Residual risk:** Sophisticated rigs with randomized timing could pass basic checks. ML-based behavioral modeling is the long-term answer — training anomaly detection on large datasets of real human phone usage to identify the subtle statistical signatures that distinguish mechanical from organic interaction.

---

#### 2.6.2 Rooted Phone Sensor Spoofing

**Attack:** Use Xposed/Magisk on rooted Android to hook all sensor APIs, returning fabricated GPS, accelerometer, Bluetooth, and touch data that looks organic.

**Severity:** High

**Why it's dangerous:**
- Can generate plausible sensor data without any physical manipulation — no mechanical rig needed
- Passes Wall 3 (real ARM chip) — the phone is genuine ARM hardware
- Only Wall 1's behavioral analysis stands in the way
- A skilled attacker can study PoL parameters and craft sensor data that precisely matches expected distributions
- Xposed hooks operate below the application layer, making them invisible to standard app-level detection

**Current mitigations:**
- Play Integrity / SafetyNet detects root — but this is optional, not required, by design (to support custom ROMs and phones without Google services, per section 2.4)

**Proposed mitigations:**
- Behavioral anomaly detection across days — cross-day behavioral consistency checks that look for the subtle correlations between sensor streams that are extremely difficult to fabricate (e.g., GPS movement should correlate with accelerometer patterns, Bluetooth peer changes should correlate with location changes)
- TEE attestation as a stronger signal — hardware-backed attestation cannot be spoofed even on a rooted phone, since the TEE operates independently of the Android OS
- Hardware-backed sensor attestation where available — Android 13+ hardware attestation API can verify that sensor readings originate from real hardware, not software hooks

**Residual risk:** A determined attacker with a rooted phone can fake most sensor data. The defense is making it statistically detectable over time, not preventing it on any single day. Over 30+ days, the correlations between fabricated sensor streams will diverge from genuine human patterns in ways that ML-based analysis can identify.

---

#### 2.6.3 Replay Attacks

**Attack:** Record a real day's sensor patterns from legitimate use, then replay that data (with minor variations) on subsequent days to pass PoL without actual human interaction.

**Severity:** High

**Why it's dangerous:**
- The replayed data IS real human data — it passed PoL once legitimately
- Minor variations (adding noise, shifting timestamps slightly) make simple hash comparison insufficient
- An attacker only needs one genuine day of phone use to generate an indefinite number of replayed attestations
- This attack requires minimal technical sophistication compared to sensor fabrication

**Current mitigations:**
- Daily PoL parameters are checked independently each day
- GPS fix must be current — stale GPS data from a previous day would show the wrong timestamp
- Bluetooth peer sets change daily in real life — replayed Bluetooth data would reference peers that may no longer be in range

**Proposed mitigations:**
- Cross-day behavioral consistency WITH variation detection — the same human should have consistent but NOT identical patterns day to day. Replayed data with minor noise lacks the natural day-to-day variation of real human behavior (e.g., different wake times, different routes, different activity levels)
- Require live Bluetooth peer discovery — peers change daily and cannot be replayed. A replayed attestation claiming the same Bluetooth peers on Day 15 as Day 1 is a strong anomaly signal
- Tie attestations to block height or chain state — include the current block hash in the attestation to prevent pre-recording. Attestation data must reference on-chain state that didn't exist when the recording was made
- Nonce-based sensor challenges — the protocol requests a specific sensor reading at an unpredictable time during the day. The node must respond with a live reading that matches the challenge parameters, making pre-recorded data useless

**Residual risk:** A sophisticated replay attack that also captures live Bluetooth and GPS data (by actually being near the phone during replay) could persist for a few days. Multi-day statistical analysis catches it because the behavioral fingerprint will lack the organic variation that real human usage exhibits over a week or more.

---

#### 2.6.4 Human-Assisted Phone Farming

**Attack:** Pay people minimum wage to carry an extra phone in their pocket and perform minimal interactions (unlock, swipe, plug in at night). Cheaper than gig economy Sybil (2.1) because the carrier doesn't need to use the phone as their primary device — just enough to pass PoL.

**Severity:** Medium (overlaps with 2.1 but lower bar for the carrier)

**Why it's dangerous:**
- Real human motion — the phone is in a real person's pocket, generating genuine accelerometer and GPS data
- Real GPS variation — the carrier moves through their normal daily life
- Real charge cycles — plugged in at night like any phone
- PoL parameters are designed for "normal use" and pocket-carrying with occasional unlocks might meet the minimum thresholds
- Lower cost than full gig economy Sybil because the carrier doesn't need to actively use the phone as their daily driver

**Current mitigations:**
- 10 unlock events across 6+ hours is a meaningful bar — the carrier must remember to interact with the second phone throughout the day
- Organic touch patterns required — quick unlock-and-lock cycles lack the gesture diversity of genuine phone use
- Varying Bluetooth environments expected — a phone in a pocket will see peers, but interaction patterns matter too

**Proposed mitigations:**
- App usage diversity signals — a phone used ONLY for Gratia has suspiciously low app diversity compared to a genuine personal device. **Caution:** this must be implemented with extreme care for privacy. Only aggregate app activity metrics (number of distinct apps used, total screen-on time), never specific app names or usage data
- Screen-on time minimums correlated with unlock patterns — unlocking 10 times but only having 30 seconds of total screen-on time is not consistent with genuine phone use
- Behavioral richness score — not just "did they unlock 10 times" but "did the interactions look like genuine phone use" across multiple dimensions: scroll depth, gesture variety, session duration distribution, time between interactions

**Residual risk:** A carrier who actually uses the second phone as a secondary device — texts, social media, casual browsing — becomes indistinguishable from a legitimate user. This is the fundamental limit of Proof of Life. The defense is that the economics must make this unprofitable: the cost of paying a carrier plus providing a phone must exceed the GRAT earned.

---

#### 2.6.5 ARM Emulators with Sensor Injection

**Attack:** Modern ARM servers (AWS Graviton, Ampere) running Android emulators with injected sensor data. Unlike x86 emulators (Tier 1.1), these run real ARM code natively.

**Severity:** Medium

**Why it's dangerous:**
- Passes Wall 3's ARM-specific computation requirement — the server chip is genuinely ARM architecture
- Can run hundreds of emulator instances per server, making the per-node cost extremely low
- ARM instruction set compatibility means the mining workload executes at full speed
- Unlike x86 emulation, there is no performance penalty or instruction translation overhead to detect

**Current mitigations:**
- Wall 3 checks big.LITTLE scheduling patterns — server ARM chips (Graviton, Ampere Altra) do NOT have big.LITTLE architecture, they use homogeneous core designs
- Phone-specific thermal signatures differ from server chips — server CPUs are water-cooled or fan-cooled with flat thermal curves, unlike phones that thermal throttle under sustained load
- No physical sensors to detect — server VMs have no GPS, accelerometer, Bluetooth, or ambient light hardware

**Proposed mitigations:**
- big.LITTLE scheduling fingerprinting — server Graviton and Ampere chips have homogeneous cores, producing fundamentally different scheduling patterns than phone SoCs with big.LITTLE or DynamIQ core arrangements. This is a strong detection signal
- Thermal curve analysis — servers don't thermal throttle like phones. A phone under sustained mining load will show characteristic thermal ramp-up and throttle-back cycles. Servers maintain flat thermal profiles. Require thermal curve data as part of attestation
- TEE attestation — server VMs fail Android Keystore/StrongBox checks. ARM servers do not have phone-grade secure enclaves, and VM-level TEE (like AWS Nitro) has a different attestation format than Android TEE
- Battery state verification — servers have no battery. Any battery data from a server VM is fabricated, making it a strong binary detection signal. Real phones report genuine charge/discharge curves that follow predictable electrochemical patterns

**Residual risk:** Low if big.LITTLE + TEE + battery checks are all implemented. ARM servers fundamentally differ from ARM phones in enough measurable ways (core topology, thermal behavior, secure enclave type, battery presence) that this attack vector is detectable with high confidence.

---

## TIER 3: Existential / State-Level Threats

### 3.1 Nation-State Suppression

**Attack:** A government with real resources decides Gratia is a threat and takes action:
- Block Gratia's network traffic at the ISP level (like China blocks VPNs)
- Compromise phone manufacturers in their jurisdiction to tamper with sensor readings at firmware level
- Criminalize running a node, reducing that country's honest node count and weakening its shard
- Seize founding team members and coerce a malicious update

**Severity:** High (within a single country), Low (globally)

**Mitigations:**
- **Bluetooth mesh** keeps the network alive locally even without internet
- **Protocol updates require governance vote** — no single team can push a malicious update
- **Open source** — the protocol can't be "seized." Anyone can fork and continue.
- **Geographic distribution** — no single country's actions kill the global network
- **PWA and sideloading fallbacks** — app store removal doesn't kill the app

**Honest assessment:** A single authoritarian government can probably suppress Gratia within its borders. But they cannot kill the global network. This is the same threat model as Bitcoin, Tor, and Signal — all of which survive despite state-level opposition.

---

### 3.2 Supply Chain / Firmware Attack

**Attack:** Compromise a popular phone manufacturer's firmware to either:
- Fake sensor readings to pass Proof of Life without a real user
- Exfiltrate private keys from the secure enclave
- Tamper with the mining process

**Severity:** Medium-High (limited to one manufacturer's devices)

**Mitigations:**
- Secure enclave keys are hardware-isolated — even a compromised OS cannot extract them on properly implemented devices
- Proof of Life cross-references multiple independent sensors — compromising one sensor type is insufficient
- Behavioral analysis compares patterns across the entire network — a batch of devices all behaving identically from the same manufacturer would be flagged
- Device diversity means no single manufacturer controls majority of nodes

**Residual risk:** A state-level actor compelling a manufacturer to backdoor the secure enclave implementation itself. This is a risk shared with all mobile security (banking apps, authentication apps, etc.) and is beyond Gratia-specific mitigation.

---

## Priority Action Items

| Priority | Action | Purpose |
|----------|--------|---------|
| ~~**P0**~~ | ~~Mathematical modeling of gig economy Sybil cost vs. network size~~ | ✅ **DONE** — See [sybil-economic-model.md](sybil-economic-model.md). Security threshold: 100K honest miners. |
| ~~**P0**~~ | ~~Design graduated committee system for early network~~ | ✅ **DONE** — See [committee-scaling.md](committee-scaling.md). 7-tier scaling from 3 to 21 validators. |
| ~~**P1**~~ | ~~Develop behavioral clustering detection algorithms~~ | ✅ **DONE** — Implemented in `gratia-pol/src/clustering.rs`. Peer graph hashing + synchronized mining detection. |
| ~~**P1**~~ | ~~Define sharding spec with minimum shard sizes and merge criteria~~ | ✅ **DONE** — See [geographic-sharding.md](geographic-sharding.md). 20% cross-shard validators, 5K min nodes, merge/split/freeze. |
| ~~**P1**~~ | ~~Implement TEE attestation as strong PoL signal~~ | ✅ **DONE** — Implemented in `gratia-pol/src/tee.rs`. Full/Basic/Failed/Absent trust levels with score adjustments. |
| ~~**P1**~~ | ~~Cross-day behavioral anomaly detection~~ | ✅ **DONE** — Implemented in `gratia-pol/src/behavioral_anomaly.rs`. 30-day rolling window, 5-signal consistency score. |
| **P1** | Fund adversarial red-teaming on testnet | Pay people to actually attempt gig economy Sybil and report findings |
| ~~**P2**~~ | ~~Bluetooth peer graph analysis~~ | ✅ **DONE** — Covered in `gratia-pol/src/clustering.rs` via PeerSetHash comparison. |
| **P2** | Establish behavioral baseline governance model | Prevent slow poisoning without centralizing baseline control |
| ~~**P2**~~ | ~~Model VRF committee selection statistics at various network sizes~~ | ✅ **DONE** — Covered in [committee-scaling.md](committee-scaling.md) capture probability tables. |
| **P3** | Publish threat model openly | Let the security community find holes before attackers do |

---

## Security Principles

1. **Assume every wall will be tested.** Design mitigations in depth — if one fails, the others must hold.
2. **Economic security, not just cryptographic security.** Many attacks are theoretically possible but economically irrational. Model the economics.
3. **Transparency over obscurity.** Publishing our threat model openly is stronger than hiding it. Attackers will find vulnerabilities regardless — the question is whether defenders find them first.
4. **The early network is the most vulnerable.** Design specifically for the first 6-12 months when node count is low and an attacker's percentage is highest.
5. **No single point of failure.** Not in the team, not in the infrastructure, not in any dependency (Google, Apple, any single country).
