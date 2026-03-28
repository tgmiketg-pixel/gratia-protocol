# HANDOFF — Session 2026-03-25

## What Was Done

### Security Hardening (Major)
This session implemented the full security hardening roadmap from design through code:

**Specifications (6 new docs):**
- threat-model.md: 5 PoL spoofing vectors (§2.6), all P0/P1 action items marked done
- security-architecture.md: PoL hardening, staking-as-insurance framework, progressive trust model
- tokenomics.md: Concrete staking params (14-day peg, 100× cap, progressive slashing, fraud reporter share)
- sybil-economic-model.md: Full economic model, security threshold = 100K honest miners
- committee-scaling.md: 7-tier graduated committee (3→21 validators), capture probability tables
- geographic-sharding.md: Full sharding spec with merge/split/freeze/beacon chain

**Code (12 modules created or heavily modified, 562 total tests):**
- committee.rs — Full rewrite with graduated scaling, cooldown, trust filtering
- slashing.rs — Progressive model (48hr/10%/50%/100%), fraud reporter share
- trust.rs — NEW: 5 trust tiers, scrutiny levels, slashing resets
- behavioral_anomaly.rs — NEW: 30-day rolling window, 5-signal consistency score
- tee.rs — NEW: TEE attestation verification, 4 trust levels, score adjustments
- clustering.rs — NEW: BT peer graph hashing, co-location detection, farm signatures
- nfc.rs — NEW: NFC tap-to-transact protocol, zero-fee threshold, session management
- scoring.rs — Enhanced with TEE + behavioral adjustments
- sharding.rs — Enhanced with health monitoring, split/merge, cross-shard validators
- validation.rs — Committee parameter validation
- lib.rs (consensus) — Trust-aware initialization, timestamp tracking
- 3 integration test files — phone farm, sybil resistance, network partition simulations

**Bug Fixes (4 from previous HANDOFF):**
- BlockHeader::hash() no longer panics (returns Result)
- VRF selection NaN safety
- Block timestamp monotonicity validation
- DailyProofOfLifeData::is_valid() uses config instead of hardcoded values

**Whitepaper Updates:**
- Sections 3.1, 4.5, 5.3, 6.3, 12.3, 12.4, 12.5, 16.3 all updated
- New: "Mine immediately, keep the privilege" onboarding model
- New: "Why Gratia Is Harder to Attack Than Any Existing Blockchain" section (12.5)

## What Remains

### Phase 1 PoC (Requires Real Devices)
1. ~~**Phone-to-phone consensus demonstration**~~ — **DONE.** Both phones producing blocks, committee exchange working, chain height advancing, mining rewards accumulating. Auto-consensus on launch (no manual setup).
2. **Rust tracing → Android logcat** — Using file-based logging workaround (`gratia-rust.log`). Still need `android_logger` crate for proper logcat integration.

### Medium Priority (Code)
1. ~~**ProofOfLifeAttestation linkability**~~ — **DONE.** Replaced with blinded_id + nullifier scheme. On-chain attestations are now unlinkable. LocalProofOfLifeRecord added for on-device use.
2. **State pruning efficiency** — `gratia-state/pruning.rs` loads all transactions into memory. Should use streaming RocksDB iterators.
3. **Peer discovery eviction** — `gratia-network/discovery.rs` uses O(n) eviction. Should use LRU cache.
4. **ViewModels duplicate data classes** — Android UI duplicates bridge types
5. ~~**GovernanceScreen create button**~~ — **DONE.** Create Poll and Create Proposal dialogs implemented with full form validation.
6. ~~**MiningService CPU temp fallback**~~ — **DONE.** Three-strategy fallback: PowerManager thermal API (API 29+) → sysfs thermal zone → conservative 45°C default.

### Phase 2 — Testnet (Ready to Start)
1. Multi-node consensus over real cellular/Wi-Fi via libp2p gossipsub
2. RocksDB state storage with pruning for mobile constraints
3. ECVRF block producer selection with Presence Score weighting
4. Staking mechanism with overflow pool (mostly done in code, needs integration)
5. Governance voting and on-chain polling in-app (code done, needs UI wiring)
6. GratiaVM WASM runtime via wasmer (code exists, needs contract deployment flow)
7. GratiaScript compiler: basic TypeScript-like syntax to WASM
8. Block explorer web app
9. Network simulation tests ✅ DONE (phone farm, Sybil, partition)

### Threat Model Status
- All P0 items: ✅ DONE
- All P1 items: ✅ DONE (except red-teaming which is operational, not code)
- P2 Bluetooth peer graph: ✅ DONE (in clustering.rs)
- P2 VRF statistics: ✅ DONE (in committee-scaling.md)
- P2 Behavioral baseline governance: NOT STARTED
- P3 Publish threat model: NOT STARTED

### Completed Today (2026-03-26, continued autonomous)
- Committee exchange protocol (gossipsub NodeAnnouncement)
- Block height progression (auto-sign with committee member IDs)
- Auto-consensus start (no manual peer connect or button tapping)
- FileKeystore (wallet key persists across restarts)
- Balance persistence (wallet_balance.bin survives restarts)
- Kotlin bridge fixed for new FFI API
- UX: NetworkViewModel always shows Connect card
- Fixed port 9000 for demo peer connections
- Chain state persistence (height, tip hash, blocks_produced survive restarts via chain_state.bin)
- Consolidated rust_log helper with OnceLock-based path caching
- UX: Connect card shows green "Connected (N)" indicator when peers connected
- mDNS discovery logging added for debugging auto-peer-connect
- Fixed dual reward system (removed per-minute tick reward, block finalization is sole source)
- Emission schedule: 269 GRAT per block (simplified Year 1 formula)
- P2P transactions: send_transfer now broadcasts via gossipsub, receiving phone credits balance
- try_broadcast_transaction_sync added to NetworkManager
- PoL state persistence (consecutive days, total days, onboarding flag via pol_state.bin)
- Consolidated rust_log with OnceLock path caching
- Emission schedule module (gratia-core/emission.rs) — Year 1: 2.125B GRAT, 25% annual reduction, per-block and per-miner calculations
- Block reward now uses real emission formula instead of hardcoded value

### Persistent Wallet Addresses
- S25: `grat:e830638f776cee71159c8b2d6fba3927824257a5f1ef4ceb452a6afd2dde22a1`
- A06: `grat:0960e2fd0023dbb060db362bf87a646d2babad3748f4783816194b350cf73943`

### Session 2026-03-27: Phase 2 Foundation

**Explorer with Real Data:**
- `build_explorer_json()` now returns real block data from the `recent_blocks` cache
- Real block hashes, timestamps, producers, transaction counts from actual chain
- Real transaction data extracted from block payloads (transfers, stakes, etc.)
- Wallet-local transactions included for pending tx visibility
- Computed average block time from actual block timestamps
- Real TPS calculated from block data
- `blocksProduced` field added to network stats

**Web Explorer Live Connection:**
- Auto-probes localhost ports (8080, 8081, 9090) for running Gratia node
- Live connection status indicator (green=Live, yellow=Reconnecting, gray=Demo)
- `?api=URL` parameter still works for manual connection (e.g., phone IP)
- 4-second auto-refresh in live mode (matches block time)
- Graceful fallback to demo data when no node detected

**Block Sync Protocol Improvements:**
- Lowered `MIN_PEERS_FOR_SYNC_DECISION` from 3 to 1 for Phase 2 testnet
- Enables sync between just 2 phones (was impossible before with 3-peer minimum)
- Updated tests to match new threshold
- Incoming blocks from network now apply transactions to on-chain StateManager
- Mining rewards credited for blocks received from other producers
- State persisted after synced blocks (every 5 blocks)

**State Persistence Hardening:**
- Increased persistence frequency from every 10 blocks to every 5 blocks
- State now saved after both produced AND synced blocks (was only produced)
- Added logging for successful state persistence
- Maximum state loss reduced from ~40 seconds to ~20 seconds

**Android UI Fixes:**
- Gratia hexagonal logo (Canvas-drawn) added to every screen's TopAppBar as navigationIcon
- Bottom nav "Governance" label: set `maxLines=1`, `fontSize=11.sp`, `TextOverflow.Ellipsis` to prevent wrapping
- Mining pulse animation: switched from `Modifier.alpha()` to `graphicsLayer {}` for hardware-accelerated animation (fixes inconsistent pulsing across Samsung devices)
- Pulse easing changed from LinearEasing to EaseOut for more organic feel
- Seed phrase export: wired through FFI (`export_seed_phrase()` → Rust `WalletManager::export_seed_phrase()` → hex string)
- `SeedPhraseDisplayDialog` added to SettingsScreen with copy-to-clipboard
- `GratiaCoreManager.exportSeedPhrase()` bridge method added

**Website (gratia.io):**
- Full landing page built at `web/site/` — zero dependencies, pure HTML/CSS/JS
- Hero section with app screenshots, stats bar (634 tests, 4s blocks, 11 crates, $50 min phone)
- Sections: Problem, How It Works, Three-Pillar Security, Hardware Moat, Specs, Comparison Table, Roadmap, Whitepaper
- Whitepaper PDF generated from v5 markdown with branded styling (navy/gold)
- Executive Summary added to whitepaper — 1-page condensed version before the full abstract
- All 3 whitepaper download buttons link to real PDF (672KB)
- GitHub link removed (no dead links)
- Mobile responsive (900px + 480px breakpoints)
- Nav with scroll-aware background blur

**Remaining:**
- Screenshots on website still show pre-logo version of app (phones were locked, couldn't capture fresh branded ones)
- Need to unlock phones → capture wallet/mining/network screens with new logo → replace 3 PNGs in `web/site/img/`
- Domain (gratia.io) not yet purchased/configured
- Hosting not yet set up (Cloudflare Pages / Netlify recommended)

**GratiaScript Compiler (crates/gratiascript/) — THE MOAT:**
- Full compiler: Lexer → Parser → AST → WASM Code Generator
- TypeScript-derived syntax with mobile-native `@` builtins
- Lexer: keywords, types, operators, @builtins, strings, numbers, comments (12 tests)
- Parser: contracts, fields, functions, params, if/else, while, emit, @store.write, binary/unary ops, field access (14 tests)
- CodeGen: valid WASM binary output with host function imports, exported functions, globals, memory, LEB128 encoding (9 tests)
- Integration: LocationTrigger, ProximityEscrow, PresenceVerifier, WeatherOracle all compile to valid WASM (8 tests)
- 4 template contracts in `contracts/templates/` (.gs files)
- Mobile-native opcodes no other blockchain has: @location, @proximity, @presence, @sensor, @blockHeight, @blockTime, @caller, @balance, @store.read, @store.write

**GratiaScript FFI Integration:**
- `compile_contract(source)` — compile .gs source to WASM hex on-device
- `compile_and_deploy_contract(source)` — compile + deploy in one step, returns contract address
- gratiascript crate added as dependency to gratia-ffi

**ECVRF Real Presence Score:**
- Replaced hardcoded `demo_score=100` with actual `presence_score` from PoL sensor data
- New nodes default to score 75 (above 40 threshold, below max) until first PoL calculation
- Debug bypass still uses 100 for testing
- Committee reconstruction uses real scores

**Block Sync in Slot Timer:**
- Every 8 slots (~32 seconds), slot timer checks sync state
- If behind network, generates sync requests to catch up
- SyncManager tracks local chain state from consensus engine

**Governance FFI Wired — One Phone, One Vote:**
- `submit_proposal(title, description)` → creates proposal with 14-day discussion + 7-day voting
- `vote_on_proposal(id, vote)` → yes/no/abstain, requires valid PoL
- `get_proposals()` → returns all proposals with vote counts and status
- `create_poll(question, options, duration)` → creates on-chain poll
- `vote_on_poll(id, option_index)` → one-phone-one-vote
- `get_polls()` → returns active polls with results
- GovernanceManager added to GratiaNodeInner
- Bridge methods added to GratiaCoreManager.kt
- GovernanceViewModel rewritten to use real Rust bridge (was mock in-memory)
- FfiProposal + FfiPoll data types added to FFI
- BridgeProposal + BridgePoll data classes added to Kotlin bridge

**Pure-Rust WASM Interpreter (gratia-vm/interpreter.rs):**
- Drop-in replacement for MockRuntime — implements ContractRuntime trait
- Parses WASM binary sections (type, import, function, memory, global, export, code)
- Stack-based execution engine for all GratiaScript opcodes (i32/i64/f32/f64 arithmetic, comparisons, control flow)
- Host function dispatch with permission checking (location, proximity, presence, sensor, block, caller, balance)
- Gas metering on every instruction
- No C/C++ dependencies — compiles for Android ARM64, iOS, any target
- 22 tests

**GratiaScript Type Checker (gratiascript/typechecker.rs):**
- Walks AST and resolves all expression types
- SymbolTable with nested scopes (push/pop on block entry)
- Binary op type compatibility (can't add i32+f32, modulo requires integers)
- Function call argument count and type checking
- Return type verification against function signatures
- @builtin return type resolution (@location→Location, @proximity→i32, etc.)
- Field access (Location.lat→f32, Location.lon→f32)
- Immutability enforcement (const field assignment rejected)
- 43 tests

**InterpreterRuntime Wired into FFI:**
- init_vm() now uses InterpreterRuntime (pure Rust) instead of MockRuntime
- Demo contracts compiled from real GratiaScript source (PresenceVerifier, ProximityGate, LocationCheck)
- Full pipeline: .gs source → compile → WASM → deploy → interpreter executes
- No more fake WASM or mock handlers for demo contracts

**End-to-End Integration Tests (15 tests):**
- Compilation: verify, proximity, arithmetic, globals, while loops, block height, host imports, multiple functions
- Deployment: simple contract, contract with functions, all 4 templates
- Error handling: unknown builtin, syntax error, unterminated string

**Codegen Type Inference Fix:**
- Added `expr_is_float()` to codegen — infers operand types from expression context
- Integer ops now use i32 opcodes (add/sub/mul/div/eq/ne/lt/gt/le/ge)
- Float ops use f32 opcodes (for GPS coords, sensor readings)
- Unary neg: i32 uses multiply-by-negative-one pattern (WASM has no i32.neg)
- Fixes interpreter type mismatch errors on integer comparisons and arithmetic
- **FULL PIPELINE VERIFIED:** GratiaScript → WASM → InterpreterRuntime → host function dispatch → correct results
- Example: `@presence()` returns 85, compared with `minScore=70`, returns true — 229 gas consumed

**Type Checker Wired into Compile Pipeline:**
- `compile()` now runs type checker before codegen: lexer → parser → **type check** → codegen
- Catches type errors at compile time (wrong types in assignments, returns, function calls)
- Added implicit widening: i32→i64 and f32→f64 are safe, no cast needed
- All contracts pass type checking

**Whitepaper + Synopsis + Website Updated:**
- Test counts: 705 (was 634)
- Crate count: 13 (was 11)
- Added GratiaScript compiler, WASM interpreter, type checker to implementation status
- Regenerated PDF (683KB), copied to website
- Website hero stats updated
- Roadmap updated (GratiaScript moved from Phase 2 to Phase 1 complete)

**APK deployed to both phones (A06 confirmed, S25 reconnected)**

**Tests:** 705 passing, 0 failures

## Build Notes
- mDNS auto-discovery CONFIRMED — phones find each other automatically on same WiFi
- Zero-touch flow proven: install → open → auto-discover → auto-consensus → blocks produced
- Rust tests: 583 total, all passing
- APK pre-built with all latest changes
- Transaction signature verification on receive (rejects forged transactions)
- Balance ledger tracks all known addresses (prevents double-spend, insufficient balance)
- Send debits ledger, receive credits after verification
- 3 new balance ledger tests
- Rust tests: 586 total, all passing
- Android cross-compile works from any path (uses NDK clang)
- Two phones connected: R9ZX90L1F2W (A06), RFCXC0N7ZCE (S25)
- Use `scripts/build-android.sh debug` for fast iteration
