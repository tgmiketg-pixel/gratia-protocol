# CLAUDE.md — Gratia Project

## Project Overview

Gratia is a mobile-native layer-1 blockchain and smart contract platform designed to run exclusively on smartphones. The protocol uses a dual-proof system (Proof of Life + Proof of Presence) with three-pillar consensus security (Life + Stake + Energy). The token ticker is GRAT (working name — may change). The project name "Gratia" is also a working name.

## Core Design Decisions (Finalized)

These decisions have been made and should not be revisited without explicit instruction:

### Dual-Proof System
- **Proof of Life (Passive Mode):** Runs in background when phone is NOT mining. Collects sensor attestation data from normal phone usage. Zero battery impact. Zero user action required.
- **Proof of Presence (Mining Mode):** Activates ONLY when phone is plugged in AND battery is at or above 80%. Flat reward rate per minute of mining. No diminishing returns. No time-of-day restrictions.

### Proof of Life Required Daily Parameters
All must be met within a rolling 24-hour window:
1. Minimum 10 unlock events spread across at least a 6-hour window
2. Screen interaction events showing organic touch patterns at multiple points throughout the day (timing/frequency only, never content)
3. At least one orientation change (phone picked up or moved)
4. Accelerometer data showing human-consistent motion during at least a portion of the day
5. At least one GPS fix confirming a plausible geographic location
6. Connection to at least one Wi-Fi network OR detection of Bluetooth peers
7. Varying Bluetooth peer environments at some point during the day (different device sets at different times)
8. At least one charge cycle event (plug-in or unplug) during the 24-hour period

### Three-Pillar Consensus Security
All three must be satisfied simultaneously:
1. **Proof of Life** — Primary wall. Stops phone farms. Cannot be bought.
2. **Staking** — Secondary layer. Stops small-scale multi-device gaming. Capped with overflow pool.
3. **Energy Expenditure** — Tertiary layer. Stops emulators/VMs. Real ARM chip, real power, real computation.

### Mining Rules
- Phone must be plugged in to any power source
- Battery must be at or above 80%
- Valid Proof of Life for the current day must exist
- Minimum stake must be in place
- Phone charges to 80% FIRST, then mining activates (user's needs always come first)
- Flat reward rate — every minute of mining earns the same. NO diminishing returns.
- Thermal management throttles workload if CPU gets too hot

### Staking Model: Capped with Overflow Pool (Option C)
- Minimum stake required to participate in mining (amount TBD, governance-adjustable)
- Per-node stake cap (e.g., 1,000 GRAT)
- Any stake above cap flows to Network Security Pool
- Pool yield distributed to ALL active mining nodes proportionally
- Whales earn yield on full staked amount but consensus power is capped
- Wealth concentration subsidizes small miners by design

### Consensus Threshold: Binary Pass/Fail
- Core four requirements: ARM processor, GPS, accelerometer, Wi-Fi OR Bluetooth
- Designed so 50%+ of existing smartphones worldwide can pass
- Target: phones $50+ manufactured after 2018
- No SIM card required. Wi-Fi-only phones are full participants.
- Once threshold is crossed, ALL nodes earn the same base mining reward
- No tiers. No levels. No earning multipliers based on phone quality.
- Composite Presence Score (40-100) exists above threshold but ONLY affects block production selection probability (security function), NOT rewards

### Governance: One Phone, One Vote
- NOT token-weighted. One verified node = one vote.
- 90+ days Proof of Life history to submit proposals
- 14-day discussion period
- 7-day voting period
- 51% of votes cast to pass
- 20% quorum of active mining nodes
- 30-day implementation delay after passage
- Emergency security patches: 75% supermajority of validator committee, must be ratified by standard vote within 90 days
- Governance system itself is subject to governance

### Wallet Security
- **Layer 1:** Secure enclave key storage (keys never leave hardware)
- **Layer 2:** Biometric authorization for every transaction
- **Layer 3:** Proof of Life behavioral binding (detects different human operating device)
- **Recovery:** Proof of Life behavioral matching over 7-14 day window on new device. Old wallet frozen during recovery. Owner can reject claim instantly from original device.
- **NO social recovery** (collusion vulnerability)
- **Optional seed phrase** (buried in settings, opt-in, not default, not shown during onboarding)
- **Optional inheritance feature** (designate beneficiary wallet, 365-day dead-man switch, opt-in)

### Onboarding
- Zero-delay onboarding. User installs app, plugs in phone, mining starts immediately. No waiting period.
- Progressive trust builds in background: Day 0 = Unverified (max scrutiny), Day 7 = Provisional, Day 30 = Established, Day 90+ = Trusted (committee/governance eligible)
- Mining rewards are flat at every trust level — what changes is trust, not earnings
- Must maintain valid Proof of Life EVERY DAY to remain mining-eligible
- 1-day grace period for missed Proof of Life. Two consecutive missed days pauses mining. Resumes immediately on next valid day.

### Token Distribution: Fair Launch
- Genesis block mined by founding team on real phones under same rules as everyone
- 10-15% founding allocation (development fund 4yr vest, team 1yr lock + 3yr vest, ecosystem grants)
- NO private investor pre-sale at a discount
- 85-90% emitted through mining only
- 25% annual emission reduction (gentler than Bitcoin's 50% halving)
- Geographic equity: underserved regions earn elevated rewards

### Tokenomics
- Ticker: GRAT (working name)
- Smallest unit: 1 Lux (1 GRAT = 1,000,000 Lux) — working name
- Maximum supply: TBD
- Transaction fees: minimal, burned (deflationary)
- Smart contract gas: denominated in Lux, based on ARM compute cycles
- NFC/Bluetooth: transport layers only (deliver signed transactions to the network, NOT offline-confirmed payments)
- Intrinsic floor value: energy cost to produce each token
- GRAT classified as a commodity (not a security)

### On-Chain Polling System
- Any GRAT holder can create a poll
- Every response comes from a Proof-of-Life-verified unique human
- One phone, one vote per poll
- Results are on-chain, publicly auditable, tamper-proof
- Use cases: protocol governance, political polling, market research, community decisions, dispute resolution
- Poll creation costs GRAT (burned)

### Smart Contract Platform: GratiaVM
- ARM-optimized bytecode
- Strict resource limits: 256 MB memory, 500ms execution time
- Mobile-native opcodes: @location, @proximity, @presence, @sensor
- Language: GratiaScript (TypeScript-derived)
- Contract types: location-triggered, proximity, presence, environmental oracle, time-zone-aware
- Testing on real phones via DevKit app

### Network Architecture
- Layer 0: Mesh (Bluetooth + Wi-Fi Direct) — transport only, relays signed transactions to network
- Layer 1: Consensus (cellular/Wi-Fi) — all transactions require BFT finality before confirmation
- Layer 2: Application (smart contracts, dApps)
- Layer 3: Oracle (sensor data aggregation)
- Block size: 256 KB
- Block time: 3-5 seconds
- Base throughput: 131-218 TPS
- Scaling: geographic sharding (10 shards = ~2,000 TPS)
- Block production: Weighted Random Selection via VRF, 21 validator committee, 14/21 (67%) finality
- Archive nodes (servers) can store history but CANNOT participate in consensus

### Transaction Finality: No Offline Payments
- **All transactions require BFT consensus confirmation.** A transaction is not final until it is included in a block with committee signatures (14/21 finality threshold).
- **No offline-confirmed payments.** NFC, Bluetooth, and Wi-Fi Direct are transport layers only — they deliver signed transactions between phones, but the transaction is NOT confirmed until the network validates it.
- **WHY:** Offline payments enable double-spend attacks. A malicious user can disconnect, send GRAT to a merchant on a local fork, reconnect, and the fork is abandoned — the merchant loses the payment. No amount of trust scoring, stake collateral, or hardware attestation can make offline confirmation trustless.
- **NFC tap-to-pay flow:** Alice taps Bob's phone → signed transaction is transferred via NFC → Bob's phone broadcasts to the network → BFT finality confirms it → Bob sees "Confirmed." Both phones need internet access (at least one must relay to the network).
- **Bluetooth mesh relay:** Phones without direct internet can relay transactions through nearby phones that DO have connectivity. The mesh is a transport bridge, not a confirmation mechanism.

### App Store Strategy
- Position as "wallet with rewards" not "crypto mining app"
- Fallback: Android sideloading, alternative app stores (F-Droid, Samsung, Huawei), TestFlight (iOS), EU alternative stores (AltStore PAL)
- Mining only when plugged in + above 80% means zero battery degradation concern

### Privacy
- All sensor data processed on-device. Raw data NEVER leaves the phone.
- Zero-knowledge proofs for all attestations
- Unlinkable attestations between days
- Camera and microphone features are strictly opt-in
- Location granularity user-controlled

## Hardware Utilization

### Core (Required for threshold — ~95% of smartphones):
- ARM CPU: consensus algorithm, anti-ASIC via big.LITTLE exploitation
- GPS: location verification (Wi-Fi-assisted OK)
- Accelerometer: human behavior verification
- Wi-Fi OR Bluetooth: connectivity and peer discovery
- Battery/charging state: mining mode controller
- Flash storage: pruned state, sharded, 2-5 GB target

### Standard (Boosts Presence Score, not required):
- Gyroscope (+5)
- Ambient light sensor (+3)
- Bluetooth peer discovery in addition to Wi-Fi (+5)
- Cellular radio + cell tower data (+8)
- Barometer (+5)
- Magnetometer (+4)
- NFC (+5)
- Secure enclave/TEE (+8)
- Fingerprint/biometric sensor (+5)
- Wi-Fi Direct (transaction relay between nearby phones)

### Enhanced (Opt-in):
- Camera environment hash (+4)
- Microphone ambient fingerprint (+4)

### Earned Over Time:
- 30+ days participation (+2)
- 90+ days participation (+2 additional)

## Technical Stack (Finalized)

### Core Protocol: Rust

Rust is the protocol implementation language. This is non-negotiable for the following reasons:

- **ARM optimization:** Rust compiles to highly optimized ARM64 native binaries via LLVM. Zero-cost abstractions mean protocol-level code runs at near-C performance on mobile ARM chipsets (Snapdragon, MediaTek, Exynos, Apple Silicon).
- **Memory safety without garbage collection:** No GC pauses during consensus operations. Predictable, deterministic performance critical for block production within 3-5 second windows on battery-constrained devices.
- **Proven in blockchain:** Solana, Polkadot/Substrate, Near, Aptos, and Sui all use Rust for their core protocol. The ecosystem of blockchain-specific Rust libraries is mature.
- **Cross-compilation:** Single Rust codebase compiles to Android (aarch64-linux-android), iOS (aarch64-apple-ios), and server targets (for archive nodes and tooling).

Key Rust crates:
- `tokio` — async runtime for networking and concurrent sensor data processing
- `serde` / `bincode` — serialization for blocks, transactions, attestations
- `rocksdb` (via `rust-rocksdb`) — state storage engine, proven ARM performance, used by many blockchains
- `ring` — core cryptography (AES, SHA, ECDSA, Ed25519). Uses ARM hardware crypto accelerators (ARMv8 Cryptography Extensions) automatically.
- `ed25519-dalek` — Ed25519 signatures for transaction signing and attestation verification
- `x25519-dalek` — key exchange for encrypted peer communication
- `libp2p` — peer-to-peer networking (peer discovery, gossipsub for block/tx propagation, NAT traversal). Used by IPFS, Filecoin, Polkadot. Handles Bluetooth and Wi-Fi transport layers.
- `rand` + `rand_chacha` — cryptographically secure randomness for VRF

### Zero-Knowledge Proofs: Bulletproofs + Optional Groth16

**Bulletproofs (default for Proof of Life attestations):**
- No trusted setup required — critical for a decentralized protocol with no central authority
- Compact proof sizes (~700 bytes for range proofs)
- Proven on ARM — Monero uses Bulletproofs and runs on mobile devices
- Rust implementation: `bulletproofs` crate (dalek-cryptography)
- Used for: Proof of Life attestations (proving parameters are met without revealing raw sensor data), range proofs on transaction amounts, geographic attestation proofs

**Optional ZK Transactions (user choice per transaction):**
- Users can choose to send a **standard transaction** (transparent — sender, receiver, and amount visible on chain, like Bitcoin) or a **shielded transaction** (ZK — proves validity without revealing amount or parties)
- Shielded transactions use Bulletproofs for amount range proofs and Pedersen commitments for hiding values
- For more complex ZK smart contract interactions, Groth16 proofs via the `bellman` crate provide fast verification (important for mobile validators) at the cost of slightly larger proving time on the sender's device
- ZK transaction creation is computationally heavier — designed to run during Mining Mode (plugged in, above 80%) so the phone has power to spare. Users can queue a ZK transaction while unplugged and the proof generates automatically next time they plug in.

**Implementation:**
```
Standard transaction: ~250 bytes, instant creation, transparent on chain
Shielded transaction: ~1.5-2 KB, 2-5 second proof generation on ARM, amount and parties hidden
```

Users choose per transaction. Default is standard (transparent). Shielded is one tap away in the UI.

### VRF (Verifiable Random Function): ECVRF

- Implementation: ECVRF per RFC 9381, built on the `curve25519-dalek` crate
- Used for: block producer selection, validator committee selection
- Weighted by Presence Score for selection probability
- Deterministic and verifiable — any node can verify that the selected producer was chosen fairly

### Mobile App: Shared Rust Core + Native UI (Android & iOS)

**Architecture: ~80% shared code, both platforms fully supported.**

Gratia runs on both Android and iOS. The architecture is designed so that the heavy lifting — all protocol logic, wallet operations, ZK proofs, consensus, networking, and state management — lives in a shared Rust core library that compiles natively to both Android ARM64 and iOS ARM64. The same Rust code, the same logic, running on both platforms.

The remaining ~20% is a thin native layer on each platform that handles two things only: reading phone sensors (which require platform-specific APIs) and drawing the UI (which uses each platform's native toolkit for the best user experience). This thin layer talks to the shared Rust core through Mozilla's UniFFI, which auto-generates both Kotlin bindings (Android) and Swift bindings (iOS) from the same Rust source.

**Why this approach instead of Flutter or React Native:**
- Deep hardware access is non-negotiable. Gratia needs direct access to secure enclaves (Android Keystore/StrongBox, iOS Secure Enclave), NFC, Bluetooth LE, accelerometer, GPS, barometer, magnetometer, ambient light, battery state, and charging detection. Cross-platform frameworks abstract away the hardware layer, introducing latency, bugs, and limitations in sensor access that are unacceptable for a protocol that depends on sensor fidelity.
- Performance for consensus operations. No JavaScript bridge, no Dart VM, no abstraction overhead — just native compiled code on both platforms.
- Best-in-class UX on both platforms. Native UI toolkits (Jetpack Compose on Android, SwiftUI on iOS) follow each platform's design conventions, making the app feel like it belongs on each device.

**Architecture diagram:**
```
ANDROID                              iOS
┌─────────────────────┐              ┌─────────────────────┐
│ Kotlin UI Layer     │              │ Swift UI Layer      │
│ (Jetpack Compose)   │              │ (SwiftUI)           │
├─────────────────────┤              ├─────────────────────┤
│ Kotlin Sensor Layer │              │ Swift Sensor Layer  │
│ (Android APIs)      │              │ (Core Motion, etc.) │
├─────────────────────┤              ├─────────────────────┤
│ UniFFI Bridge       │              │ UniFFI Bridge       │
│ (auto-gen Kotlin)   │              │ (auto-gen Swift)    │
├─────────────────────┴──────────────┴─────────────────────┤
│                SHARED RUST CORE (~80%)                    │
│  Protocol, consensus, wallet, ZK proofs, networking,     │
│  state management, transaction logic, PoL validation     │
├──────────────────────────────────────────────────────────┤
│           Hardware Security Layer                        │
│  Android Keystore/StrongBox  |  iOS Secure Enclave      │
└──────────────────────────────────────────────────────────┘
```

- `mozilla/uniffi-rs` — generates Kotlin AND Swift bindings from the same Rust code automatically. Battle-tested by Mozilla Firefox on both Android and iOS.
- **Android Phase 1:** Kotlin sensor layer + Jetpack Compose UI + shared Rust core
- **iOS Phase 2:** Swift sensor layer + SwiftUI + same shared Rust core (no Rust rewrite needed)
- Adding iOS is primarily writing the Swift sensor managers and SwiftUI screens — the protocol, wallet, ZK proofs, consensus, and networking are already done in Rust.

**Android UI:** Jetpack Compose with Material Design 3
**iOS UI:** SwiftUI with native iOS design conventions
**Both platforms:** Minimal screens — wallet balance, transaction history, mining status, settings, governance. Basic but robust. No visual clutter, no unnecessary features, rock-solid core functionality.

### Smart Contract VM: WASM-based

**GratiaVM runs WebAssembly:**
- WASM is the industry standard for blockchain VMs (Polkadot, Near, Cosmos/CosmWasm all use WASM)
- Excellent ARM performance — `wasmer` and `wasmtime` runtimes both support ARM64 natively
- Sandboxed execution — contracts cannot access host resources outside their allocation
- Deterministic — same input always produces same output across all nodes
- GratiaScript (TypeScript-derived) compiles to WASM via a custom compiler
- Mobile-native opcodes (@location, @proximity, @presence, @sensor) are implemented as WASM host functions that the VM exposes to contracts

**Runtime:** `wasmer` (Rust-native, ARM64 optimized, used in production by multiple blockchains)

### Networking: libp2p

- `rust-libp2p` for all peer-to-peer communication
- **Gossipsub** for block and transaction propagation (efficient pub/sub for mobile networks)
- **Kademlia DHT** for peer discovery
- **QUIC transport** as primary (better than TCP for mobile networks — handles connection migration when switching between Wi-Fi and cellular)
- **Bluetooth transport** via custom libp2p transport adapter for mesh layer
- **Noise protocol** for encrypted peer connections

### State Storage: RocksDB

- `rust-rocksdb` — proven, battle-tested, excellent ARM performance
- Used by Ethereum (geth), Solana, CockroachDB, and many others
- Optimized for SSD/flash storage (critical for mobile NAND flash)
- Tunable for mobile constraints (memory-mapped I/O limits, write buffer sizes, compaction scheduling)
- Target state database: 2-5 GB maximum

### Cryptographic Primitives Summary

| Function | Algorithm | Crate | ARM Hardware Accel |
|----------|-----------|-------|-------------------|
| Transaction signing | Ed25519 | `ed25519-dalek` | No (fast in software) |
| Key exchange | X25519 | `x25519-dalek` | No (fast in software) |
| Hashing | SHA-256 / SHA-512 | `ring` | Yes (ARMv8-CE) |
| Symmetric encryption | AES-256-GCM | `ring` | Yes (ARMv8-CE) |
| Proof of Life ZK | Bulletproofs | `bulletproofs` | No (optimized in software) |
| Shielded transactions | Bulletproofs + Pedersen | `bulletproofs` | No |
| Complex ZK (contracts) | Groth16 | `bellman` | No (proving is heavy, verification is fast) |
| VRF | ECVRF (Curve25519) | `curve25519-dalek` | No (fast in software) |
| Merkle trees | SHA-256 based | `ring` | Yes (ARMv8-CE) |
| Peer encryption | Noise Protocol | `snow` | Partial (AES portion) |

### Build and Cross-Compilation

```bash
# Android ARM64 target
rustup target add aarch64-linux-android
cargo build --target aarch64-linux-android --release

# iOS ARM64 target (Phase 2)
rustup target add aarch64-apple-ios
cargo build --target aarch64-apple-ios --release

# Archive node / tooling (server)
cargo build --target x86_64-unknown-linux-gnu --release
```

Android NDK required for Android builds. UniFFI generates Kotlin bindings at build time.

## Project Structure

```
gratia/
├── CLAUDE.md                        # This file
├── Cargo.toml                       # Rust workspace root
├── docs/
│   ├── whitepaper-v0.4.md          # Current whitepaper
│   ├── technical-spec/              # Detailed technical specifications
│   └── research/                    # Research notes and references
│
├── crates/                          # Rust workspace members
│   ├── gratia-core/                 # Core protocol types, traits, config
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── gratia-consensus/            # Proof of Presence consensus engine
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── block_production.rs
│   │       ├── validation.rs
│   │       ├── vrf.rs               # ECVRF block producer selection
│   │       └── committee.rs         # 21-validator committee logic
│   ├── gratia-pol/                  # Proof of Life attestation engine
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── attestation.rs       # Daily attestation builder
│   │       ├── parameters.rs        # PoL parameter validation
│   │       ├── behavioral.rs        # Behavioral pattern matching (recovery)
│   │       └── scoring.rs           # Composite Presence Score
│   ├── gratia-staking/              # Capped staking with overflow pool
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── pool.rs              # Network Security Pool
│   │       ├── slashing.rs          # Three-pillar slashing
│   │       └── rewards.rs           # Flat-rate mining reward distribution
│   ├── gratia-wallet/               # Wallet and key management
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── keystore.rs          # Secure enclave key management
│   │       ├── transactions.rs      # Transaction creation and signing
│   │       ├── shielded.rs          # ZK shielded transactions (optional)
│   │       └── recovery.rs          # PoL behavioral matching recovery
│   ├── gratia-zk/                   # Zero-knowledge proof system
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── bulletproofs.rs      # PoL attestation proofs
│   │       ├── shielded_tx.rs       # Shielded transaction proofs
│   │       ├── pedersen.rs          # Pedersen commitments
│   │       └── groth16.rs           # Complex ZK for smart contracts
│   ├── gratia-vm/                   # GratiaVM (WASM-based smart contracts)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── runtime.rs           # Wasmer WASM runtime
│   │       ├── host_functions.rs    # Mobile-native opcodes (@location etc)
│   │       ├── gas.rs               # ARM compute cycle gas metering
│   │       └── sandbox.rs           # Contract sandboxing and limits
│   ├── gratia-network/              # Peer-to-peer networking
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── gossip.rs            # Gossipsub block/tx propagation
│   │       ├── discovery.rs         # Kademlia DHT peer discovery
│   │       ├── transport.rs         # QUIC + Bluetooth transports
│   │       └── sync.rs              # State synchronization
│   ├── gratia-state/                # Blockchain state management
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── db.rs                # RocksDB state storage
│   │       ├── merkle.rs            # Merkle tree state roots
│   │       ├── pruning.rs           # State pruning for mobile storage
│   │       └── sharding.rs          # Geographic shard management
│   ├── gratia-governance/           # One-phone-one-vote governance
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── proposals.rs         # Proposal submission and lifecycle
│   │       ├── voting.rs            # Vote casting and tallying
│   │       └── polling.rs           # On-chain polling system
│   └── gratia-ffi/                  # UniFFI bindings for mobile
│       ├── Cargo.toml
│       ├── src/
│       │   └── lib.rs               # UniFFI interface definitions
│       └── uniffi/
│           └── gratia.udl           # UniFFI definition language file
│
├── app/
│   ├── android/                     # Android application (Kotlin)
│   │   ├── build.gradle.kts
│   │   └── src/main/
│   │       ├── kotlin/
│   │       │   ├── ui/              # Jetpack Compose UI
│   │       │   │   ├── WalletScreen.kt
│   │       │   │   ├── MiningScreen.kt
│   │       │   │   ├── SettingsScreen.kt
│   │       │   │   └── GovernanceScreen.kt
│   │       │   ├── sensors/         # Android sensor access
│   │       │   │   ├── GpsManager.kt
│   │       │   │   ├── AccelerometerManager.kt
│   │       │   │   ├── BluetoothManager.kt
│   │       │   │   ├── WifiManager.kt
│   │       │   │   ├── NfcManager.kt
│   │       │   │   ├── BatteryManager.kt
│   │       │   │   ├── BarometerManager.kt
│   │       │   │   ├── MagnetometerManager.kt
│   │       │   │   └── LightSensorManager.kt
│   │       │   ├── service/         # Background services
│   │       │   │   ├── ProofOfLifeService.kt
│   │       │   │   └── MiningService.kt
│   │       │   └── bridge/          # UniFFI Rust bridge
│   │       │       └── GratiaCore.kt
│   │       └── res/                 # Android resources
│   └── ios/                         # iOS application (Swift) — Phase 2
│       ├── GratiaApp.xcodeproj
│       └── Sources/
│           ├── UI/                  # SwiftUI screens
│           │   ├── WalletView.swift
│           │   ├── MiningView.swift
│           │   ├── SettingsView.swift
│           │   └── GovernanceView.swift
│           ├── Sensors/             # iOS sensor access
│           │   ├── GpsManager.swift         # Core Location
│           │   ├── AccelerometerManager.swift # Core Motion
│           │   ├── BluetoothManager.swift   # Core Bluetooth
│           │   ├── WifiManager.swift        # NEHotspotHelper
│           │   ├── NfcManager.swift         # Core NFC
│           │   ├── BatteryManager.swift     # UIDevice battery
│           │   ├── BarometerManager.swift   # CMAltimeter
│           │   ├── MagnetometerManager.swift # Core Motion
│           │   └── LightSensorManager.swift # Ambient light
│           ├── Service/             # Background services
│           │   ├── ProofOfLifeService.swift
│           │   └── MiningService.swift
│           └── Bridge/              # UniFFI Rust bridge
│               └── GratiaCore.swift # Auto-generated Swift bindings
│
├── contracts/
│   ├── gratiascript/                # GratiaScript to WASM compiler
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── templates/                   # Standard contract templates
│   │   ├── location_trigger.gs
│   │   ├── proximity_escrow.gs
│   │   ├── presence_verification.gs
│   │   └── poll.gs                  # On-chain polling contract
│   └── devkit/                      # Developer testing tools
│
├── web/
│   ├── site/                        # Project website (gratia.io)
│   └── explorer/                    # Block explorer
│
└── tests/
    ├── unit/                        # Per-crate unit tests
    ├── integration/                 # Cross-crate integration tests
    ├── simulation/                  # Multi-node network simulation
    │   ├── phone_farm_attack.rs     # Simulate phone farm and verify PoL catches it
    │   ├── sybil_resistance.rs      # Simulate various Sybil attack vectors
    │   └── network_partition.rs     # Simulate connectivity failures
    └── security/                    # Attack scenario testing
        ├── behavioral_spoofing.rs   # Attempt to fake PoL patterns
        ├── emulator_detection.rs    # Verify emulators fail attestation
        └── stake_manipulation.rs    # Test overflow pool edge cases
```

## Development Phases

### Phase 1 — Proof of Concept (Current Priority)
1. Set up Rust workspace with crate structure (gratia-core, gratia-pol, gratia-wallet, gratia-ffi)
2. Implement Proof of Life parameter collection in Kotlin via Android sensor APIs
3. Build UniFFI bridge between Kotlin sensor layer and Rust core
4. Implement Rust-side PoL attestation validation (parameter checking, behavioral analysis)
5. Implement Mining Mode activation logic (plugged in + 80% battery detection)
6. Basic wallet: Ed25519 key generation in Android Keystore/StrongBox, send/receive transactions
7. Bulletproofs integration for PoL zero-knowledge attestations
8. Phone-to-phone consensus demonstration via libp2p (2-3 devices on same Wi-Fi)
9. NFC tap-to-relay prototype (NFC delivers signed transaction to recipient's phone, which broadcasts to network for BFT confirmation — no offline-only payments)
10. Basic Jetpack Compose UI: wallet balance, mining status, transaction history
11. Optional shielded transaction: Bulletproofs + Pedersen commitment for hidden amounts

### Phase 2 — Testnet
1. Multi-node consensus over real cellular/Wi-Fi via libp2p gossipsub
2. RocksDB state storage with pruning for mobile constraints
3. ECVRF block producer selection with Presence Score weighting
4. Staking mechanism with overflow pool (gratia-staking crate)
5. Governance voting and on-chain polling in-app (gratia-governance crate)
6. GratiaVM WASM runtime via wasmer (gratia-vm crate)
7. GratiaScript compiler: basic TypeScript-like syntax to WASM
8. Block explorer web app
9. Network simulation tests (phone farm attack, Sybil resistance, partition tolerance)

### Phase 3 — Mainnet
1. Full GratiaVM with mobile-native host functions (@location, @proximity, @presence, @sensor)
2. Geographic sharding implementation
3. iOS app via Swift + same Rust core (UniFFI Swift bindings)
4. Bluetooth mesh transport adapter for libp2p
5. Wi-Fi Direct transaction relay layer (phone-to-phone transport, NOT offline confirmation)
6. Environmental oracle layer (aggregated barometer, light, magnetometer data)
7. Groth16 proofs for complex ZK smart contract interactions
8. Security audit of Rust core, ZK proofs, and consensus mechanism
9. App store submission as "Gratia Wallet" (wallet with rewards framing)

## Key Principles for Development

1. **Phone-first everything.** If it can't run on a $50 phone from 2018, redesign it.
2. **User experience is invisible.** The user should never need to understand blockchain to use Gratia. Install, use phone normally, plug in, earn.
3. **Privacy by default.** Raw sensor data never leaves the device. Zero-knowledge proofs for all attestations.
4. **Honest about limitations.** Don't overpromise throughput, security, or capabilities. Be transparent.
5. **Fair by design.** Every design decision should be tested against: "Does this benefit a wealthy user more than a poor user?" If yes, redesign it.
6. **No SIM dependency.** Never assume cellular connectivity. Wi-Fi-only must be a first-class citizen.
7. **Battery health sacred.** Never degrade the user's device. Mining only when plugged in + above 80%. Thermal throttling mandatory.

## Reference Documents

- Whitepaper: docs/whitepaper-v0.4.md
- Bitcoin whitepaper (reference): https://bitcoin.org/bitcoin.pdf
- Monero RandomX (reference for ASIC-resistance philosophy)
- ARM big.LITTLE architecture documentation
- Android sensor API documentation
- iOS Core Motion / Core Location documentation
- Android Keystore / StrongBox documentation
- iOS Secure Enclave documentation

### Rust Ecosystem References
- UniFFI: https://mozilla.github.io/uniffi-rs/
- libp2p Rust: https://github.com/libp2p/rust-libp2p
- dalek-cryptography (Ed25519, X25519, Bulletproofs): https://github.com/dalek-cryptography
- ring (crypto primitives): https://github.com/briansmith/ring
- wasmer (WASM runtime): https://wasmer.io/
- RocksDB Rust: https://github.com/rust-rocksdb/rust-rocksdb
- bellman (Groth16): https://github.com/zkcrypto/bellman
- Jetpack Compose: https://developer.android.com/jetpack/compose
- Android NDK Rust: https://developer.android.com/ndk/guides

### Blockchain Architecture References
- Substrate/Polkadot (Rust blockchain framework): https://substrate.io/
- CosmWasm (WASM smart contracts): https://cosmwasm.com/
- Solana architecture docs (Rust blockchain reference)
- RFC 9381 (ECVRF specification)
- Bulletproofs paper: https://eprint.iacr.org/2017/1066
