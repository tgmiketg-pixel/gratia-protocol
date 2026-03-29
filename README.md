# Gratia

**A mobile-native Layer 1 blockchain where your phone is the miner and your daily life is the proof of work.**

[![Rust](https://img.shields.io/badge/Rust-2021_Edition-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Android](https://img.shields.io/badge/Android-Kotlin_+_Jetpack_Compose-3DDC84?logo=android&logoColor=white)](https://developer.android.com/)
[![License](https://img.shields.io/badge/License-MIT_OR_Apache--2.0-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/Tests-934%2B_passing-brightgreen.svg)](#test-coverage)

---

## What is Gratia?

Gratia is a Layer 1 blockchain whose consensus mechanism is architecturally dependent on mobile hardware -- GPS, accelerometers, Bluetooth, NFC, and secure enclaves -- that servers and mining rigs simply do not have. Only real phones held by real people can mine. No amount of capital can bypass it. The protocol uses a dual-proof system (Proof of Life + Proof of Presence) with three-pillar consensus security, one-phone-one-vote governance, and a built-in decentralized social protocol where every account is a verified unique human.

**The protocol is running on real phones today.** Two Android devices -- a $90 Samsung A06 and a flagship S25 -- produce blocks, send verified transactions, discover peers automatically, and reach consensus over the internet.

---

## Key Features

- **Proof of Life** -- Normal phone usage (unlocking, carrying, charging) passively generates cryptographic attestation that you are a real human. Zero battery impact. Zero user action. Phone farms and emulators cannot pass.

- **Mine by plugging in your phone** -- When your phone is plugged in and above 80% battery, it mines GRAT at a flat rate. Every minute pays the same. No diminishing returns. Your phone charges first -- user needs always come first.

- **One phone, one vote governance** -- Not token-weighted. A user with 1 GRAT has the same governance power as a user with 1 million. Proposals require 90+ days of Proof of Life history to submit, 14-day discussion, 7-day voting, and 51% majority to pass.

- **Lux social protocol** -- A decentralized public feed where every account is PoL-verified. Bot armies are impossible. Fake engagement is impossible. No corporation controls the feed. On-chain text posts with gossipsub propagation.

- **Zero-knowledge privacy** -- All sensor data processed on-device. Raw data never leaves the phone. Bulletproofs ZK attestations for Proof of Life. Optional shielded transactions hide sender, receiver, and amount.

- **ARM-optimized for $50 phones** -- Runs on any smartphone manufactured after 2018 with GPS, accelerometer, and Wi-Fi or Bluetooth. No SIM card required. Wi-Fi-only phones are full participants.

- **GratiaScript smart contracts** -- TypeScript-derived language with mobile-native opcodes (`@location`, `@proximity`, `@presence`, `@sensor`) that no server-based blockchain can offer. Compiles to WASM.

- **Fair launch** -- 85-90% of tokens emitted through mining only. No private presale at a discount. Geographic equity rewards underserved regions.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                      ANDROID APPLICATION                         │
│  ┌─────────────────────┐  ┌────────────────────────────────┐    │
│  │   Jetpack Compose    │  │      Kotlin Sensor Layer       │    │
│  │   (Material 3 UI)    │  │  GPS, Accel, BT, Wi-Fi, NFC,  │    │
│  │                      │  │  Battery, Barometer, Light...  │    │
│  └──────────┬──────────┘  └───────────────┬────────────────┘    │
│             └──────────┬──────────────────┘                      │
│                        ▼                                         │
│              ┌─────────────────┐                                 │
│              │  UniFFI Bridge   │  (auto-generated Kotlin)       │
│              └────────┬────────┘                                 │
└───────────────────────┼──────────────────────────────────────────┘
                        ▼
┌──────────────────────────────────────────────────────────────────┐
│                    SHARED RUST CORE (~80%)                        │
│                                                                  │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌──────────┐ ┌────────┐  │
│  │Consensus │ │Proof of  │ │ Wallet │ │   ZK     │ │  Lux   │  │
│  │  + VRF   │ │  Life    │ │Ed25519 │ │Bulletprfs│ │ Social │  │
│  └──────────┘ └──────────┘ └────────┘ └──────────┘ └────────┘  │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌──────────┐ ┌────────┐  │
│  │ Staking  │ │Governance│ │GratiaVM│ │  State   │ │GratiaS.│  │
│  │ + Pool   │ │ + Polls  │ │  WASM  │ │ RocksDB  │ │Compiler│  │
│  └──────────┘ └──────────┘ └────────┘ └──────────┘ └────────┘  │
│                                                                  │
│              ┌─────────────────────────────┐                     │
│              │   libp2p Network Layer       │                    │
│              │  Gossipsub + Kademlia + QUIC │                    │
│              └─────────────────────────────┘                     │
└──────────────────────────────────────────────────────────────────┘
```

---

## Crate Structure

The Rust workspace contains 15 crates:

| Crate | Description |
|-------|-------------|
| `gratia-core` | Core protocol types, traits, configuration, and cryptographic primitives |
| `gratia-pol` | Proof of Life attestation engine -- 8-parameter daily validation and behavioral analysis |
| `gratia-consensus` | Block production, ECVRF producer selection, 21-validator committee consensus |
| `gratia-wallet` | Ed25519 key management, transaction signing, optional shielded transactions |
| `gratia-zk` | Zero-knowledge proofs -- Bulletproofs for PoL attestations, Pedersen commitments |
| `gratia-staking` | Capped staking with overflow pool, flat-rate reward distribution, progressive slashing |
| `gratia-governance` | One-phone-one-vote governance, proposal lifecycle, on-chain polling |
| `gratia-vm` | GratiaVM smart contract engine -- WASM sandbox with mobile-native host functions |
| `gratia-network` | libp2p networking -- gossipsub, Kademlia DHT, QUIC transport, peer sync |
| `gratia-state` | RocksDB state storage, Merkle trees, pruning for mobile storage constraints |
| `gratia-lux` | Decentralized social protocol -- verified-human posts, dynamic fees, moderation |
| `gratia-ffi` | UniFFI bridge generating Kotlin (and future Swift) bindings from Rust |
| `gratiascript` | GratiaScript-to-WASM compiler -- lexer, parser, type checker, code generator |
| `gratia-bootstrap` | Bootstrap server and initial peer discovery for new nodes joining the network |
| `gratia-tests` | Cross-crate integration tests, network simulations, and security attack scenarios |

---

## Getting Started

### Prerequisites

- **Rust** (stable, 2021 edition) -- [Install](https://rustup.rs/)
- **Android NDK** -- Required for ARM cross-compilation
- **JDK 17** -- Required for Android builds
- **Android SDK** -- With build tools and platform for target API level

### Build the Rust Core

```bash
# Build all crates (host target, for development and testing)
cargo build

# Run all tests
cargo test
```

### Build for Android ARM64

```bash
# Add the Android ARM target
rustup target add aarch64-linux-android

# Build the Rust core for Android (requires NDK)
bash scripts/build-android.sh
```

> **Important:** When Rust code changes, you must run `build-android.sh` before building the APK. A Gradle-only build will not pick up Rust changes.

### Build the Android APK

```bash
cd app/android
./gradlew assembleDebug
```

### Run on a Connected Device

```bash
adb install app/android/build/outputs/apk/debug/app-debug.apk
```

---

## Test Coverage

The protocol is backed by **934+ automated tests** across the full workspace:

| Category | Tests | What They Validate |
|----------|------:|--------------------|
| Core protocol types & crypto | 86 | Ed25519, SHA-256, transaction serialization, address derivation |
| Proof of Life validation | 102 | All 8 PoL parameters, behavioral analysis, suspicious pattern detection |
| Staking & overflow pool | 54 | Caps, overflow distribution, cooldown, slashing |
| Consensus & VRF | 61 | Committee selection, block production, VRF proofs, graduated scaling |
| Wallet & transactions | 45 | Key generation, signing, verification, shielded transactions |
| Network & gossip | 34 | Gossipsub, deduplication, peer discovery, sync protocol |
| GratiaScript compiler | 86 | Lexer tokenization, parser AST, WASM codegen, type checking |
| WASM interpreter | 17 | Instruction execution, host functions, gas metering, control flow |
| Smart contract e2e | 19 | Compile, deploy, execute pipeline with template contracts |
| Governance | 53 | Proposal lifecycle, voting mechanics, polls, one-phone-one-vote |
| GratiaVM & sandbox | 49 | Contract deployment, gas accounting, permissions, resource limits |
| Lux social protocol | 21 | Post creation, dynamic fees, feed ordering, gossip propagation |
| Phone farm attack sim | 18 | Scripted unlock detection, narrow spread rejection |
| Sybil resistance | 13 | Stake caps, whale power capping, slash/ban enforcement |
| Behavioral spoofing | 10 | Emulator TEE penalties, presence score boundaries |
| Stake manipulation | 10 | Zero-stake rejection, exact-cap overflow, yield distribution |
| State pruning | 12 | Block pruning, snapshot retention, storage reclamation |
| Mempool & replay protection | 15 | Signature verification, chain_id validation, nonce ordering |

```bash
cargo test
```

---

## Performance

| Metric | Value |
|--------|-------|
| Block time | 4 seconds |
| Base throughput | 128 TPS (single chain) |
| With geographic sharding | 512 -- 2,560 TPS (4-20 shards) |
| Transaction size | ~200 bytes |
| Max block size | 256 KB |
| Consensus finality | 14/21 validator signatures (67%) |
| App startup to first block | ~15 seconds |

---

## Contributing

Contributions are welcome. To get started:

1. Fork the repository and create a feature branch.
2. Ensure `cargo build` completes with zero warnings.
3. Ensure `cargo test` passes all 934+ tests.
4. Write tests for any new functionality.
5. Submit a pull request with a clear description of the change.

Please keep changes focused -- one logical change per PR. For large changes, open an issue first to discuss the approach.

---

## License

Licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.

---

## Links

- **Whitepaper:** [`docs/whitepaper-v0.4.md`](docs/whitepaper-v0.4.md)
- **Repository:** [github.com/gratia-network/gratia](https://github.com/gratia-network/gratia)
