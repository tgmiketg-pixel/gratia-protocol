import Foundation

// ============================================================================
// Bridge to the Rust core via UniFFI-generated Swift bindings.
//
// This singleton manages the lifecycle of the GratiaNode (the Rust-side entry
// point) and exposes its methods to the Swift UI and service layers.
//
// All Rust FFI calls are wrapped with error handling that maps FfiException
// variants to GratiaBridgeError for the Swift callers.
// ============================================================================

/// Error thrown by the bridge layer when an FFI operation fails.
enum GratiaBridgeError: Error, LocalizedError {
    case notInitialized
    case ffiError(String)

    var errorDescription: String? {
        switch self {
        case .notInitialized:
            return "Rust core not initialized. Call initialize() first."
        case .ffiError(let message):
            return message
        }
    }
}

// ============================================================================
// Bridge data types — mirror the FFI types from gratia-ffi/src/lib.rs
// ============================================================================

/// Wallet information for the UI layer. Mirrors FfiWalletInfo from the Rust FFI.
struct WalletInfo {
    let address: String
    let balanceLux: Int64
    let miningState: String

    /// Balance formatted as GRAT with 6 decimal places.
    var balanceGrat: String {
        let whole = balanceLux / 1_000_000
        let fractional = abs(balanceLux % 1_000_000)
        return "\(whole).\(String(format: "%06d", fractional))"
    }
}

/// Transaction record for the UI layer. Mirrors FfiTransactionInfo from the Rust FFI.
struct TransactionInfo: Identifiable {
    let hashHex: String
    let direction: String
    let counterparty: String?
    let amountLux: Int64
    let timestampMillis: Int64
    let status: String

    var id: String { hashHex }
}

/// Mining status for the UI layer. Mirrors FfiMiningStatus from the Rust FFI.
struct MiningStatus {
    let state: String
    let batteryPercent: Int
    let isPluggedIn: Bool
    let currentDayPolValid: Bool
    let presenceScore: Int
}

/// Proof of Life status for the UI layer. Mirrors FfiProofOfLifeStatus from the Rust FFI.
struct ProofOfLifeStatus {
    let isValidToday: Bool
    let consecutiveDays: Int64
    let isOnboarded: Bool
    let parametersMet: [String]
}

/// Staking information for the UI layer. Mirrors FfiStakeInfo from the Rust FFI.
struct StakeInfo {
    let nodeStakeLux: Int64
    let overflowAmountLux: Int64
    let totalCommittedLux: Int64
    let stakedAtMillis: Int64
    let meetsMinimum: Bool
}

/// Consensus status for the UI layer. Mirrors FfiConsensusStatus from the Rust FFI.
struct ConsensusStatus {
    let state: String
    let currentSlot: Int64
    let currentHeight: Int64
    let isCommitteeMember: Bool
    let blocksProduced: Int64
}

/// Network status for the UI layer. Mirrors FfiNetworkStatus from the Rust FFI.
struct NetworkStatus {
    let isRunning: Bool
    let peerCount: Int
    let listenAddress: String?
}

/// Network events delivered from the Rust core to the UI.
/// Mirrors FfiNetworkEvent from the Rust FFI.
enum NetworkEvent {
    case peerConnected(peerId: String)
    case peerDisconnected(peerId: String)
    case blockReceived(height: Int64, producer: String)
    case transactionReceived(hashHex: String)
}

/// Result of a smart contract execution.
struct ContractResult {
    let success: Bool
    let returnValue: String
    let gasUsed: Int64
    let gasRemaining: Int64
    let events: [String]
    let error: String?
}

/// Governance proposal for the UI layer.
struct BridgeProposal: Identifiable {
    let idHex: String
    let title: String
    let description: String
    let status: String
    let votesYes: Int64
    let votesNo: Int64
    let votesAbstain: Int64
    let discussionEndMillis: Int64
    let votingEndMillis: Int64
    let submittedBy: String

    var id: String { idHex }
}

/// On-chain poll for the UI layer.
struct BridgePoll: Identifiable {
    let idHex: String
    let question: String
    let options: [String]
    let votes: [Int64]
    let totalVoters: Int64
    let endMillis: Int64
    let createdBy: String

    var id: String { idHex }
}

/// Sensor events submitted from iOS sensor managers to the Rust PoL engine.
/// Mirrors FfiSensorEvent from the Rust FFI.
enum SensorEvent {
    case unlock
    case interaction(durationSecs: UInt32)
    case orientationChange
    case motion
    case gpsUpdate(lat: Float, lon: Float)
    case wifiScan(bssidHashes: [UInt64])
    case bluetoothScan(peerHashes: [UInt64])
    case chargeEvent(isCharging: Bool)

    /// Human-readable type string for logging.
    var type: String {
        switch self {
        case .unlock: return "unlock"
        case .interaction: return "interaction"
        case .orientationChange: return "orientation_change"
        case .motion: return "motion"
        case .gpsUpdate: return "gps_update"
        case .wifiScan: return "wifi_scan"
        case .bluetoothScan: return "bluetooth_scan"
        case .chargeEvent: return "charge_event"
        }
    }

    /// Convert this bridge-layer sensor event to the UniFFI-generated FFI type.
    /// WHY: The UniFFI Swift bindings will generate FfiSensorEvent enum variants.
    /// This method maps our Swift-friendly types to the FFI types at the boundary.
    func toFfi() -> Any {
        // WHY: Returns Any because the actual FfiSensorEvent type is auto-generated
        // by UniFFI at build time. In production, this will return FfiSensorEvent.
        // The actual mapping will be:
        //   .unlock -> FfiSensorEvent.unlock
        //   .interaction(n) -> FfiSensorEvent.interaction(durationSecs: n)
        //   .orientationChange -> FfiSensorEvent.orientationChange
        //   .motion -> FfiSensorEvent.motion
        //   .gpsUpdate(lat, lon) -> FfiSensorEvent.gpsUpdate(lat: lat, lon: lon)
        //   .wifiScan(hashes) -> FfiSensorEvent.wifiScan(bssidHashes: hashes)
        //   .bluetoothScan(hashes) -> FfiSensorEvent.bluetoothScan(peerHashes: hashes)
        //   .chargeEvent(charging) -> FfiSensorEvent.chargeEvent(isCharging: charging)
        return self
    }
}

// ============================================================================
// GratiaCoreManager — Singleton bridge to the Rust core
// ============================================================================

/// Thread-safe singleton managing all interactions with the Rust core via UniFFI.
///
/// Mirrors the Android `GratiaCoreManager` object exactly. All FFI calls are
/// wrapped with error handling and type conversion.
///
/// WHY @MainActor: SwiftUI views observe published properties on the main thread.
/// Making the manager @MainActor ensures all state mutations happen on the main
/// thread without requiring explicit dispatch.
@MainActor
final class GratiaCoreManager {

    /// Shared singleton instance.
    static let shared = GratiaCoreManager()

    /// Whether the Rust core has been successfully initialized.
    /// UI code should check this before calling core methods.
    private(set) var isInitialized: Bool = false

    /// The UniFFI-generated Rust node instance.
    /// WHY: Typed as Any? because the actual GratiaNode type is auto-generated
    /// by UniFFI at build time. In production, this will be GratiaNode?.
    private var node: AnyObject? = nil

    private let logger = GratiaLogger(tag: "GratiaCoreManager")

    private init() {}

    // ========================================================================
    // Initialization
    // ========================================================================

    /// Initialize the Rust core.
    ///
    /// Called once from the app's initialization. Creates the GratiaNode with
    /// the app's private data directory.
    ///
    /// - Parameter dataDir: Absolute path to the app's Documents directory.
    func initialize(dataDir: String) throws {
        guard !isInitialized else {
            logger.warning("Rust core already initialized, ignoring duplicate call")
            return
        }

        do {
            // WHY: In production, this will be:
            //   node = try GratiaNode(dataDir: dataDir)
            // The GratiaNode class is auto-generated by UniFFI from gratia-ffi.
            // For now, we log the initialization attempt.
            logger.info("Initializing Rust core with dataDir: \(dataDir)")

            // Placeholder for UniFFI binding:
            // node = try GratiaNode(dataDir: dataDir)

            isInitialized = true
            logger.info("GratiaCoreManager initialized (Rust core loaded)")
        } catch {
            logger.error("Failed to initialize Rust core: \(error.localizedDescription)")
            throw GratiaBridgeError.ffiError("Failed to initialize Rust core: \(error.localizedDescription)")
        }
    }

    // ========================================================================
    // Debug methods
    // ========================================================================

    /// Enable debug bypass for PoL and staking checks.
    /// Allows testing mining and transactions without waiting 24 hours for PoL.
    func enableDebugBypass() throws {
        try callNode { _ in
            // node.enableDebugBypass()
        }
        logger.info("Debug bypass enabled")
    }

    // ========================================================================
    // Wallet methods
    // ========================================================================

    /// Generate a new wallet keypair.
    /// - Returns: Wallet address as "grat:<hex>" string.
    func createWallet() throws -> String {
        return try callNode { _ in
            // return node.createWallet()
            return "grat:0000000000000000000000000000000000000000000000000000000000000000"
        }
    }

    /// Get current wallet information (address, balance, mining state).
    func getWalletInfo() throws -> WalletInfo {
        return try callNode { _ in
            // let ffi = node.getWalletInfo()
            // return WalletInfo(address: ffi.address, balanceLux: Int64(ffi.balanceLux), miningState: ffi.miningState)
            return WalletInfo(
                address: "grat:0000000000000000000000000000000000000000000000000000000000000000",
                balanceLux: 0,
                miningState: "proof_of_life"
            )
        }
    }

    /// Send a GRAT transfer to another address.
    /// - Parameters:
    ///   - to: Recipient address as "grat:<hex>" string.
    ///   - amountLux: Transfer amount in Lux (1 GRAT = 1,000,000 Lux).
    /// - Returns: Transaction hash as hex string.
    func sendTransfer(to: String, amountLux: Int64) throws -> String {
        return try callNode { _ in
            // return node.sendTransfer(to: to, amountLux: UInt64(amountLux))
            return ""
        }
    }

    /// Get the transaction history for this wallet.
    func getTransactionHistory() throws -> [TransactionInfo] {
        return try callNode { _ in
            // let ffiList = node.getTransactionHistory()
            // return ffiList.map { ... }
            return []
        }
    }

    /// Export the wallet's seed phrase as a hex string.
    func exportSeedPhrase() throws -> String {
        return try callNode { _ in
            // return node.exportSeedPhrase()
            return ""
        }
    }

    // ========================================================================
    // Mining methods
    // ========================================================================

    /// Get the current mining status.
    func getMiningStatus() throws -> MiningStatus {
        return try callNode { _ in
            // let ffi = node.getMiningStatus()
            // return ffi.toBridge()
            return MiningStatus(
                state: "proof_of_life",
                batteryPercent: 0,
                isPluggedIn: false,
                currentDayPolValid: false,
                presenceScore: 0
            )
        }
    }

    /// Update the phone's power state from the native battery manager.
    func updatePowerState(isPluggedIn: Bool, batteryPercent: Int) throws -> MiningStatus {
        return try callNode { _ in
            // let ffi = node.updatePowerState(isPluggedIn: isPluggedIn, batteryPercent: UInt8(batteryPercent))
            // return ffi.toBridge()
            return MiningStatus(
                state: isPluggedIn && batteryPercent >= 80 ? "pending_activation" : "proof_of_life",
                batteryPercent: batteryPercent,
                isPluggedIn: isPluggedIn,
                currentDayPolValid: false,
                presenceScore: 0
            )
        }
    }

    /// Request to start mining.
    func startMining() throws -> MiningStatus {
        return try callNode { _ in
            // let ffi = node.startMining()
            // return ffi.toBridge()
            return MiningStatus(
                state: "mining",
                batteryPercent: 100,
                isPluggedIn: true,
                currentDayPolValid: true,
                presenceScore: 40
            )
        }
    }

    /// Tick mining rewards for one minute of active mining.
    /// - Returns: Updated wallet balance in Lux.
    func tickMiningReward() throws -> Int64 {
        return try callNode { _ in
            // return Int64(node.tickMiningReward())
            return 0
        }
    }

    /// Stop mining.
    func stopMining() throws -> MiningStatus {
        return try callNode { _ in
            // let ffi = node.stopMining()
            // return ffi.toBridge()
            return MiningStatus(
                state: "proof_of_life",
                batteryPercent: 100,
                isPluggedIn: true,
                currentDayPolValid: true,
                presenceScore: 40
            )
        }
    }

    // ========================================================================
    // Proof of Life methods
    // ========================================================================

    /// Get the current Proof of Life status.
    func getProofOfLifeStatus() throws -> ProofOfLifeStatus {
        return try callNode { _ in
            // let ffi = node.getProofOfLifeStatus()
            // return ProofOfLifeStatus(...)
            return ProofOfLifeStatus(
                isValidToday: false,
                consecutiveDays: 0,
                isOnboarded: false,
                parametersMet: []
            )
        }
    }

    /// Submit a sensor event from the native sensor managers.
    func submitSensorEvent(_ event: SensorEvent) throws {
        try callNode { _ in
            // let ffiEvent = event.toFfi() as! FfiSensorEvent
            // node.submitSensorEvent(event: ffiEvent)
        }
    }

    /// Finalize the current day's Proof of Life.
    /// - Returns: True if the day was valid (all PoL parameters met).
    func finalizeDay() throws -> Bool {
        return try callNode { _ in
            // return node.finalizeDay()
            return false
        }
    }

    // ========================================================================
    // Staking methods
    // ========================================================================

    /// Stake GRAT for mining eligibility.
    func stake(amountLux: Int64) throws -> String {
        return try callNode { _ in
            // return node.stake(amountLux: UInt64(amountLux))
            return ""
        }
    }

    /// Unstake GRAT (subject to cooldown period).
    func unstake(amountLux: Int64) throws -> String {
        return try callNode { _ in
            // return node.unstake(amountLux: UInt64(amountLux))
            return ""
        }
    }

    /// Get current staking information for this node.
    func getStakeInfo() throws -> StakeInfo {
        return try callNode { _ in
            // let ffi = node.getStakeInfo()
            // return StakeInfo(...)
            return StakeInfo(
                nodeStakeLux: 0,
                overflowAmountLux: 0,
                totalCommittedLux: 0,
                stakedAtMillis: 0,
                meetsMinimum: false
            )
        }
    }

    // ========================================================================
    // Network methods
    // ========================================================================

    /// Start the peer-to-peer network layer.
    func startNetwork(listenPort: Int = 0) throws -> NetworkStatus {
        return try callNode { _ in
            // let ffi = node.startNetwork(listenPort: UInt16(listenPort))
            // return NetworkStatus(...)
            return NetworkStatus(isRunning: true, peerCount: 0, listenAddress: nil)
        }
    }

    /// Stop the peer-to-peer network layer.
    func stopNetwork() throws {
        try callNode { _ in
            // node.stopNetwork()
        }
    }

    /// Connect to a remote peer by multiaddr string.
    func connectPeer(addr: String) throws {
        try callNode { _ in
            // node.connectPeer(addr: addr)
        }
    }

    /// Get the current network status.
    func getNetworkStatus() throws -> NetworkStatus {
        return try callNode { _ in
            // let ffi = node.getNetworkStatus()
            // return NetworkStatus(...)
            return NetworkStatus(isRunning: false, peerCount: 0, listenAddress: nil)
        }
    }

    /// Poll for network events.
    func pollNetworkEvents() throws -> [NetworkEvent] {
        return try callNode { _ in
            // let ffiEvents = node.pollNetworkEvents()
            // return ffiEvents.map { ... }
            return []
        }
    }

    /// Start the block explorer HTTP API.
    func startExplorerApi(port: Int = 8080) throws -> String {
        return try callNode { _ in
            // return node.startExplorerApi(port: UInt16(port))
            return "http://localhost:\(port)"
        }
    }

    // ========================================================================
    // Consensus methods
    // ========================================================================

    /// Start the consensus engine and slot timer.
    func startConsensus() throws -> ConsensusStatus {
        return try callNode { _ in
            // let ffi = node.startConsensus()
            // return ffi.toBridge()
            return ConsensusStatus(
                state: "active",
                currentSlot: 0,
                currentHeight: 0,
                isCommitteeMember: false,
                blocksProduced: 0
            )
        }
    }

    /// Stop the consensus engine.
    func stopConsensus() throws {
        try callNode { _ in
            // node.stopConsensus()
        }
    }

    /// Get the current consensus status.
    func getConsensusStatus() throws -> ConsensusStatus {
        return try callNode { _ in
            // let ffi = node.getConsensusStatus()
            // return ffi.toBridge()
            return ConsensusStatus(
                state: "stopped",
                currentSlot: 0,
                currentHeight: 0,
                isCommitteeMember: false,
                blocksProduced: 0
            )
        }
    }

    /// Request block sync from connected peers.
    func requestSync() throws -> String {
        return try callNode { _ in
            // return node.requestSync()
            return "synced"
        }
    }

    // ========================================================================
    // Smart Contract methods
    // ========================================================================

    /// Initialize the GratiaVM with built-in demo contracts.
    func initVm() throws -> [String] {
        return try callNode { _ in
            // return node.initVm()
            return []
        }
    }

    /// Call a smart contract function.
    func callContract(contractAddress: String, functionName: String, gasLimit: Int64 = 1_000_000) throws -> ContractResult {
        return try callNode { _ in
            // let ffi = node.callContract(...)
            // return ContractResult(...)
            return ContractResult(
                success: false,
                returnValue: "",
                gasUsed: 0,
                gasRemaining: gasLimit,
                events: [],
                error: "VM not initialized"
            )
        }
    }

    /// List deployed contracts.
    func listContracts() throws -> [String] {
        return try callNode { _ in
            // return node.listContracts()
            return []
        }
    }

    // ========================================================================
    // Governance methods — One Phone, One Vote
    // ========================================================================

    func submitProposal(title: String, description: String) throws -> String {
        return try callNode { _ in
            // return node.submitProposal(title: title, description: description)
            return ""
        }
    }

    func voteOnProposal(proposalIdHex: String, vote: String) throws {
        try callNode { _ in
            // node.voteOnProposal(proposalIdHex: proposalIdHex, vote: vote)
        }
    }

    func getProposals() throws -> [BridgeProposal] {
        return try callNode { _ in
            // let ffiList = node.getProposals()
            // return ffiList.map { ... }
            return []
        }
    }

    // WHY: Default poll duration is 7 days (604800 seconds), matching the
    // governance voting period defined in the protocol specification.
    func createPoll(question: String, options: [String], durationSecs: Int64 = 604800) throws -> String {
        return try callNode { _ in
            // return node.createPoll(question: question, options: options, durationSecs: UInt64(durationSecs))
            return ""
        }
    }

    func voteOnPoll(pollIdHex: String, optionIndex: Int) throws {
        try callNode { _ in
            // node.voteOnPoll(pollIdHex: pollIdHex, optionIndex: UInt32(optionIndex))
        }
    }

    func getPolls() throws -> [BridgePoll] {
        return try callNode { _ in
            // let ffiList = node.getPolls()
            // return ffiList.map { ... }
            return []
        }
    }

    // ========================================================================
    // Private helpers
    // ========================================================================

    /// Execute a block against the initialized GratiaNode, mapping FFI errors.
    private func callNode<T>(_ block: (AnyObject) throws -> T) throws -> T {
        guard isInitialized, let n = node else {
            throw GratiaBridgeError.notInitialized
        }
        do {
            return try block(n)
        } catch let error as GratiaBridgeError {
            throw error
        } catch {
            logger.error("FFI error: \(error.localizedDescription)")
            throw GratiaBridgeError.ffiError(error.localizedDescription)
        }
    }
}

// ============================================================================
// Simple logger matching Android's Log.i/w/e pattern
// ============================================================================

struct GratiaLogger {
    let tag: String

    func info(_ message: String) {
        print("[\(tag)] INFO: \(message)")
    }

    func warning(_ message: String) {
        print("[\(tag)] WARN: \(message)")
    }

    func error(_ message: String) {
        print("[\(tag)] ERROR: \(message)")
    }

    func debug(_ message: String) {
        #if DEBUG
        print("[\(tag)] DEBUG: \(message)")
        #endif
    }
}
