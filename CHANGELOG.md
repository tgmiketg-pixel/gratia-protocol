# Changelog

## [Unreleased] — 2026-03-25

### Security Hardening (Major)

#### Graduated Committee Scaling (gratia-consensus)
- **Replaces:** Fixed 21-validator committee
- **New:** 7-tier committee scaling curve (3→5→7→11→15→19→21 validators) based on network size
- **Features:** Cooldown tracking, progressive trust filtering (30+ day PoL required for committee eligibility), epoch-based transitions, upward-only sizing with 7-day delayed downsizing
- **Files:** `committee.rs` (full rewrite), `block_production.rs`, `validation.rs`, `lib.rs`

#### Progressive Slashing Model (gratia-staking)
- **Replaces:** 5%/25%/100% slash schedule
- **New:** 48hr warning pause → 10% minor → 50% major + 30-day lockout → 100% permanent ban
- **Features:** 90-day rolling offense window, fraud reporter share (70% burn / 30% to validators), warning now pauses mining
- **File:** `slashing.rs`

#### Progressive Trust Model (gratia-pol) — NEW MODULE
- 5 trust tiers: Unverified → Provisional → Establishing → Established → Trusted
- Mining from Day 0 ("mine immediately, keep the privilege")
- Committee eligibility at 30+ days, governance at 90+ days
- Scrutiny levels decrease with trust, reset on slashing
- **File:** `trust.rs`

#### Behavioral Anomaly Detection (gratia-pol) — NEW MODULE
- 30-day rolling behavioral fingerprint
- 5-signal consistency score (temporal, variation, richness, movement, BT diversity)
- Anomaly flags: replay detection, behavioral discontinuity, static device, low interaction
- **File:** `behavioral_anomaly.rs`

#### TEE Attestation Integration (gratia-pol) — NEW MODULE
- 4 trust levels: Full (+8), Basic (+5), Absent (-8), Failed (-15)
- Verifies device integrity, app integrity, root/emulator detection, hardware sensor attestation
- Scrutiny modifiers: Standard, Elevated, Maximum
- **File:** `tee.rs`

#### Behavioral Clustering Detection (gratia-pol) — NEW MODULE
- Bluetooth peer graph hashing (privacy-preserving)
- Co-location detection via peer set overlap
- Synchronized mining pattern detection
- Farm signature alerts when both signals present
- **File:** `clustering.rs`

#### Enhanced Presence Score (gratia-pol)
- TEE adjustment (-15 to +8) and behavioral consistency bonus (0-10) integrated
- EnhancedPresenceScore struct with full component breakdown
- **File:** `scoring.rs`

#### Committee Parameter Validation (gratia-consensus)
- Blocks now validated against graduated scaling spec
- Wrong committee size or finality threshold for reported network size = rejection
- **File:** `validation.rs`

#### Enhanced Sharding (gratia-state)
- Split/merge criteria with persistence thresholds
- Shard health monitoring (Healthy/Warning/Critical)
- Cross-shard validator allocation (20% rule)
- Sharding activation threshold (10K nodes)
- Boundary jitter for attack mitigation
- **File:** `sharding.rs`

### NFC Tap-to-Transact (gratia-wallet) — NEW MODULE
- 3-step NFC payment protocol (PaymentRequest → PaymentConfirmation → Acknowledgment)
- Zero-fee below 10 GRAT threshold
- Session management with 30-second timeout, replay-protected nonces
- Bincode serialization for NFC data exchange
- **File:** `nfc.rs`

### Bug Fixes
- **BlockHeader::hash()** now returns `Result` instead of panicking via `.expect()`
- **VRF selection** NaN safety — degenerate values produce `f64::MAX` instead of NaN
- **Block timestamp monotonicity** — validation now rejects blocks with timestamps before previous block
- **DailyProofOfLifeData::is_valid()** now uses `ProofOfLifeConfig` instead of hardcoded thresholds

### Simulation Tests — NEW
- `phone_farm_simulation.rs` — 5 tests verifying farm node exclusion, trust filtering, capture probability
- `sybil_resistance.rs` — 6 tests verifying tier progression, finality thresholds, score weighting
- `network_partition.rs` — 5 tests verifying partition resilience, recovery, graduated committee behavior

### Specifications — NEW
- `sybil-economic-model.md` — Security threshold: 100K honest miners
- `committee-scaling.md` — 7-tier graduated committee spec with capture probability tables
- `geographic-sharding.md` — Full sharding spec with merge/split/freeze/audit/beacon chain

### Whitepaper Updates
- Section 3.1/4.5/16.3: "Mine immediately, keep the privilege" onboarding
- Section 5.3: Progressive slashing with offense table
- Section 6.3: 14-day mining peg minimum stake formula
- Section 12.3: PoL hardening (5 attack vectors, 3 defense layers)
- Section 12.4: Staking as security amplifier
- Section 12.5: "Why Gratia Is Harder to Attack Than Any Existing Blockchain"

### Infrastructure
- Fixed port 9000 for demo network (was random port 0)
- Wired network→consensus: received blocks feed into `process_incoming_block`
- Wired consensus→network: produced blocks broadcast via `try_broadcast_block_sync`
- Wired sync protocol into FFI: `request_sync()` method, SyncManager lifecycle
- Fixed GovernanceScreen create button (poll + proposal dialogs)
- Fixed MiningService CPU thermal fallback (3-strategy: PowerManager → sysfs → 45°C)
- Synced inner whitepaper with main copy
- Zero compiler warnings across workspace

### Persistence (Phase 2 prep)
- FileKeystore: wallet key persists to `wallet_key.bin` across app restarts
- Balance persistence: mining rewards saved to `wallet_balance.bin`
- Chain state persistence: height + tip hash + blocks_produced saved to `chain_state.bin`
- PoL state persistence: consecutive days + total days + onboarding flag saved to `pol_state.bin`
- ConsensusEngine.restore_state() method for loading persisted chain state

### Protocol Correctness
- Emission schedule module (gratia-core/emission.rs) — Year 1: 2.125B GRAT, 25% annual reduction
- Block rewards now use real emission formula (per_miner_block_reward_lux)
- Fixed dual reward system — removed per-minute tick, block finalization is sole reward source
- P2P transactions — send_transfer broadcasts via gossipsub, receiving phone credits balance
- try_broadcast_transaction_sync for non-blocking transaction propagation

### Test Count
- Previous: 446 tests
- Current: 583 tests (+137)

---

## [Unreleased] — 2026-03-17

### Critical Bug Fixes

#### Wi-Fi-Only Phone Validation (gratia-core, gratia-pol)
- **Problem:** Phones without Bluetooth (Wi-Fi-only devices) failed Proof of Life validation
  even though the spec states "Wi-Fi-only phones are full participants." The BT environment
  variation check (`distinct_bt_environments >= 2`) was unconditionally required, blocking
  any device that reported 0 BT environments.
- **Fix:** Changed BT variation check to: `distinct_bt_environments == 0 || distinct_bt_environments >= 2`.
  Devices with no Bluetooth pass automatically; devices that report BT peers still need variation.
- **Files:** `crates/gratia-core/src/types.rs` (line 412), `crates/gratia-pol/src/validator.rs` (line 123)
- **Tests updated:** `test_wifi_only_passes_connectivity`, `test_no_network_connectivity`, `test_proof_of_life_validation`

#### Governance Majority Vote Calculation (gratia-core)
- **Problem:** `Proposal::passed()` used `votes_yes > total_votes / 2` which suffers from
  integer truncation. With an even number of votes (e.g., 100), only 50 votes were needed
  (50%) instead of the spec-required 51%. With 4 votes, only 2 were needed instead of 3.
- **Fix:** Changed to `votes_yes * 2 > total_votes` which correctly implements strict majority
  without integer division truncation.
- **File:** `crates/gratia-core/src/types.rs` (line 561)

#### Block Height Validation for Genesis (gratia-state)
- **Problem:** `apply_block()` skipped the height validation check entirely when
  `current_height == 0` (empty chain). This allowed a genesis block with any arbitrary
  height (e.g., 999) to be accepted as the first block.
- **Fix:** Changed to always validate `block.header.height == current_height + 1`.
  Genesis block must now be height 1.
- **File:** `crates/gratia-state/src/lib.rs` (line 93)

### Compiler Warning Cleanup

Eliminated all compiler warnings across the entire Rust workspace (was ~30+ warnings, now 0):

- **gratia-core/types.rs:** Removed unused imports `SigningKey`, `Signature` from `ed25519_dalek`
- **gratia-wallet/keystore.rs:** Removed unused imports `Signature`, `Verifier`, `Sha256`, `Digest`
  (kept `Signer` which is needed for `.sign()`)
- **gratia-consensus:** Cleaned up unused imports across `block_production.rs`, `committee.rs`,
  `validation.rs`, `lib.rs` via `cargo fix`; added proper test-scoped imports for
  `COMMITTEE_SIZE` and `SLOTS_PER_EPOCH`
- **gratia-network/sync.rs:** Removed unused variable `to` in sync response handler;
  added `#[allow(dead_code)]` on `CHAIN_TIP_POLL_INTERVAL_SECS` (Phase 2 constant)
- **gratia-state/pruning.rs:** Removed unused variable `cutoff_height`
- **gratia-vm:** Removed unused imports `ExecutionOutcome`, `SandboxedExecution` and variables
  `host_env`, `permissions`; added `#[allow(dead_code)]` on `DeployedContract::address`
- **gratia-governance:** Cleaned up unused import `COMMITTEE_SIZE` in test code
- **gratia-ffi:** Added `#[allow(dead_code)]` on `GratiaNode::data_dir` (Phase 2 field)
- **gratia-consensus/lib.rs:** Added `#[allow(dead_code)]` on `started_at` (Phase 2 field)

#### Transaction Status in Phase 1 (gratia-wallet)
- **Problem:** Transactions created via `send_transfer` were always set to `Pending` status.
  With no consensus engine running (Phase 1), no block production exists to confirm them,
  so transactions stayed "pending" forever in the UI.
- **Fix:** Transactions are now immediately marked as `Confirmed` in Phase 1 since the balance
  is deducted locally and there's no block to wait for. In Phase 2, this will revert to
  `Pending` to wait for actual block inclusion.
- **File:** `crates/gratia-wallet/src/lib.rs` (lines 191, 227, 259)
- **Note:** Cross-device transactions (phone A sends to phone B) are NOT yet supported.
  Each phone runs independent state. This requires P2P networking + consensus (Phase 2).

### Android App

- Rebuilt native library (`libgratia_ffi.so`) with all Rust core fixes
- Regenerated UniFFI Kotlin bindings
- Verified app runs without crashes on Samsung Galaxy A06 (budget) and Samsung Galaxy S25 (flagship)
- Confirmed sensor data collection: GPS, accelerometer, Bluetooth, Wi-Fi all reporting
- Confirmed mining service activates correctly when plugged in with battery >= 80%

### Test Results

All 446 unit tests pass across 11 crates. Zero compiler warnings.
