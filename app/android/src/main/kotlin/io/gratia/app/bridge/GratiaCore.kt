package io.gratia.app.bridge

import android.util.Log
import uniffi.gratia_ffi.GratiaNode
import uniffi.gratia_ffi.FfiException
import uniffi.gratia_ffi.FfiConsensusStatus
import uniffi.gratia_ffi.FfiMeshStatus
import uniffi.gratia_ffi.FfiMiningStatus
import uniffi.gratia_ffi.FfiNetworkEvent
import uniffi.gratia_ffi.FfiNetworkStatus
import uniffi.gratia_ffi.FfiProofOfLifeStatus
import uniffi.gratia_ffi.FfiSensorEvent
import uniffi.gratia_ffi.FfiShardInfo
import uniffi.gratia_ffi.FfiStakeInfo
import uniffi.gratia_ffi.FfiTransactionInfo
import uniffi.gratia_ffi.FfiVmInfo
import uniffi.gratia_ffi.FfiWalletInfo

/**
 * Bridge to the Rust core via UniFFI.
 *
 * This singleton manages the lifecycle of the [GratiaNode] (the Rust-side entry
 * point) and exposes its methods to the Kotlin UI and service layers.
 *
 * All Rust FFI calls are wrapped with error handling that maps [FfiException]
 * variants to [GratiaBridgeException] for the Kotlin callers.
 */
object GratiaCoreManager {

    private const val TAG = "GratiaCoreManager"

    /**
     * Whether the Rust core has been successfully initialized.
     * UI code should check this before calling core methods.
     */
    @Volatile
    var isInitialized: Boolean = false
        private set

    /** The UniFFI-generated Rust node instance. */
    private var node: GratiaNode? = null

    // ========================================================================
    // Initialization
    // ========================================================================

    /**
     * Initialize the Rust core.
     *
     * Called once from [io.gratia.app.GratiaApplication.onCreate].
     * Creates the GratiaNode with the app's private data directory.
     *
     * @param dataDir Absolute path to the app's internal files directory.
     */
    fun initialize(dataDir: String) {
        if (isInitialized) {
            Log.w(TAG, "Rust core already initialized, ignoring duplicate call")
            return
        }

        try {
            node = GratiaNode(dataDir)
            isInitialized = true
            Log.i(TAG, "GratiaCoreManager initialized (Rust core loaded)")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to initialize Rust core", e)
            throw GratiaBridgeException("Failed to initialize Rust core: ${e.message}", e)
        }
    }

    // ========================================================================
    // Debug methods
    // ========================================================================

    /**
     * Enable debug bypass for PoL and staking checks.
     * Allows testing mining and transactions without waiting 24 hours for PoL.
     */
    fun enableDebugBypass() {
        callNode { it.enableDebugBypass() }
        Log.i(TAG, "Debug bypass enabled")
    }

    // ========================================================================
    // Wallet methods
    // ========================================================================

    /**
     * Generate a new wallet keypair.
     *
     * @return Wallet address as "grat:<hex>" string.
     * @throws GratiaBridgeException if wallet already exists or core not initialized.
     */
    fun createWallet(): String {
        return callNode { it.createWallet() }
    }

    /**
     * Get current wallet information (address, balance, mining state).
     *
     * @return Wallet info containing address, balance, and mining state.
     */
    fun getWalletInfo(): WalletInfo {
        val ffi = callNode { it.getWalletInfo() }
        return WalletInfo(
            address = ffi.address,
            balanceLux = ffi.balanceLux.toLong(),
            miningState = ffi.miningState,
        )
    }

    /**
     * Send a GRAT transfer to another address.
     *
     * @param to Recipient address as "grat:<hex>" string.
     * @param amountLux Transfer amount in Lux (1 GRAT = 1,000,000 Lux).
     * @return Transaction hash as hex string.
     */
    fun sendTransfer(to: String, amountLux: Long): String {
        return callNode { it.sendTransfer(to, amountLux.toULong()) }
    }

    /**
     * Get the transaction history for this wallet.
     *
     * @return List of transaction records.
     */
    fun getTransactionHistory(): List<TransactionInfo> {
        val ffiList = callNode { it.getTransactionHistory() }
        return ffiList.map { ffi ->
            TransactionInfo(
                hashHex = ffi.hashHex,
                direction = ffi.direction,
                counterparty = ffi.counterparty,
                amountLux = ffi.amountLux.toLong(),
                timestampMillis = ffi.timestampMillis,
                status = ffi.status,
            )
        }
    }

    /**
     * Export the wallet's seed phrase as a hex string.
     *
     * The hex string encodes the raw Ed25519 private key. In production this
     * would be converted to a BIP39 24-word mnemonic.
     *
     * @return Hex-encoded seed phrase string.
     * @throws GratiaBridgeException if wallet not initialized.
     */
    fun exportSeedPhrase(): String {
        return callNode { it.exportSeedPhrase() }
    }

    /**
     * Import a wallet from a hex-encoded seed phrase.
     *
     * Replaces the current wallet with the one derived from the seed phrase.
     * Used for wallet restoration from a backed-up seed.
     *
     * @param seedHex Hex-encoded 32-byte Ed25519 private key (64 hex chars).
     * @return New wallet address as "grat:<hex>" string.
     * @throws GratiaBridgeException if the hex is invalid or import fails.
     */
    fun importSeedPhrase(seedHex: String): String {
        return callNode { it.importSeedPhrase(seedHex) }
    }

    // ========================================================================
    // Mining methods
    // ========================================================================

    /**
     * Get the current mining status.
     *
     * @return Mining status containing state, battery, PoL validity, etc.
     */
    fun getMiningStatus(): MiningStatus {
        return callNode { it.getMiningStatus() }.toBridge()
    }

    /**
     * Update the phone's power state from the native battery manager.
     *
     * Called whenever the charging state or battery level changes.
     *
     * @param isPluggedIn Whether the phone is connected to power.
     * @param batteryPercent Current battery percentage (0-100).
     * @return Updated mining status.
     */
    fun updatePowerState(isPluggedIn: Boolean, batteryPercent: Int): MiningStatus {
        return callNode {
            it.updatePowerState(isPluggedIn, batteryPercent.toUByte())
        }.toBridge()
    }

    /**
     * Request to start mining.
     *
     * Mining will only activate if all conditions are met:
     * plugged in, battery >= 80%, valid PoL, minimum stake.
     *
     * @return Updated mining status.
     * @throws GratiaBridgeException if conditions are not met.
     */
    fun startMining(): MiningStatus {
        return callNode { it.startMining() }.toBridge()
    }

    /**
     * Tick mining rewards for one minute of active mining.
     *
     * Called by the MiningService every 60 seconds. Credits the wallet
     * with the flat-rate mining reward (1 GRAT/minute in Phase 1).
     *
     * @return Updated wallet balance in Lux.
     */
    fun tickMiningReward(): Long {
        return callNode { it.tickMiningReward().toLong() }
    }

    /**
     * Stop mining. Reverts to Proof of Life passive collection mode.
     *
     * @return Updated mining status.
     */
    fun stopMining(): MiningStatus {
        return callNode { it.stopMining() }.toBridge()
    }

    // ========================================================================
    // Proof of Life methods
    // ========================================================================

    /**
     * Get the current Proof of Life status.
     *
     * @return PoL status with validity, consecutive days, and parameter completion.
     */
    fun getProofOfLifeStatus(): ProofOfLifeStatus {
        val ffi = callNode { it.getProofOfLifeStatus() }
        return ProofOfLifeStatus(
            isValidToday = ffi.isValidToday,
            consecutiveDays = ffi.consecutiveDays.toLong(),
            isOnboarded = ffi.isOnboarded,
            parametersMet = ffi.parametersMet,
        )
    }

    /**
     * Submit a sensor event from the native sensor managers.
     *
     * Called by the Android sensor managers whenever a relevant event occurs.
     * Events are buffered in the Rust core and processed into the daily PoL
     * attestation.
     *
     * @param event The sensor event to submit.
     */
    fun submitSensorEvent(event: SensorEvent) {
        val ffiEvent = event.toFfi()
        callNode { it.submitSensorEvent(ffiEvent) }
    }

    /**
     * Finalize the current day's Proof of Life.
     *
     * Called at end-of-day (midnight UTC). Evaluates accumulated sensor data
     * and generates the PoL attestation.
     *
     * @return True if the day was valid (all PoL parameters met).
     */
    fun finalizeDay(): Boolean {
        return callNode { it.finalizeDay() }
    }

    // ========================================================================
    // Staking methods
    // ========================================================================

    /**
     * Stake GRAT for mining eligibility.
     *
     * If total committed stake exceeds the per-node cap, excess flows to
     * the Network Security Pool.
     *
     * @param amountLux Amount to stake in Lux.
     * @return Transaction hash as hex string.
     */
    fun stake(amountLux: Long): String {
        return callNode { it.stake(amountLux.toULong()) }
    }

    /**
     * Unstake GRAT (subject to cooldown period).
     *
     * @param amountLux Amount to unstake in Lux.
     * @return Transaction hash as hex string.
     */
    fun unstake(amountLux: Long): String {
        return callNode { it.unstake(amountLux.toULong()) }
    }

    /**
     * Get current staking information for this node.
     *
     * @return Staking info with effective stake, overflow, and minimum status.
     */
    fun getStakeInfo(): StakeInfo {
        val ffi = callNode { it.getStakeInfo() }
        return StakeInfo(
            nodeStakeLux = ffi.nodeStakeLux.toLong(),
            overflowAmountLux = ffi.overflowAmountLux.toLong(),
            totalCommittedLux = ffi.totalCommittedLux.toLong(),
            stakedAtMillis = ffi.stakedAtMillis,
            meetsMinimum = ffi.meetsMinimum,
        )
    }

    // ========================================================================
    // Network methods
    // ========================================================================

    /**
     * Start the peer-to-peer network layer.
     *
     * Initializes the libp2p swarm with QUIC transport, Gossipsub for
     * block/transaction propagation, and mDNS for local peer discovery.
     *
     * @param listenPort UDP port to listen on (0 = OS-assigned).
     * @return Current network status.
     */
    fun startNetwork(listenPort: Int = 0): NetworkStatus {
        val ffi = callNode { it.startNetwork(listenPort.toUShort()) }
        return NetworkStatus(
            isRunning = ffi.isRunning,
            peerCount = ffi.peerCount.toInt(),
            listenAddress = ffi.listenAddress,
        )
    }

    /**
     * Stop the peer-to-peer network layer.
     */
    fun stopNetwork() {
        callNode { it.stopNetwork() }
    }

    /**
     * Connect to a remote peer by multiaddr string.
     *
     * For local WiFi demo, use: "/ip4/<peer-ip>/udp/<port>/quic-v1"
     *
     * @param addr Multiaddr string of the peer to connect to.
     */
    fun connectPeer(addr: String) {
        callNode { it.connectPeer(addr) }
    }

    /**
     * Get the current network status.
     *
     * @return Network status with running state, peer count, and listen address.
     */
    fun getNetworkStatus(): NetworkStatus {
        val ffi = callNode { it.getNetworkStatus() }
        return NetworkStatus(
            isRunning = ffi.isRunning,
            peerCount = ffi.peerCount.toInt(),
            listenAddress = ffi.listenAddress,
        )
    }

    /**
     * Poll for network events.
     *
     * Returns a list of events that have occurred since the last poll.
     * Call periodically (e.g., every 500ms) from the UI layer.
     *
     * @return List of network events (peer connections, received blocks, etc.).
     */
    fun pollNetworkEvents(): List<NetworkEvent> {
        val ffiEvents = callNode { it.pollNetworkEvents() }
        return ffiEvents.map { ffi ->
            when (ffi) {
                is uniffi.gratia_ffi.FfiNetworkEvent.PeerConnected ->
                    NetworkEvent.PeerConnected(ffi.peerId)
                is uniffi.gratia_ffi.FfiNetworkEvent.PeerDisconnected ->
                    NetworkEvent.PeerDisconnected(ffi.peerId)
                is uniffi.gratia_ffi.FfiNetworkEvent.BlockReceived ->
                    NetworkEvent.BlockReceived(ffi.height.toLong(), ffi.producer)
                is uniffi.gratia_ffi.FfiNetworkEvent.TransactionReceived ->
                    NetworkEvent.TransactionReceived(ffi.hashHex)
            }
        }
    }

    /**
     * Start the block explorer HTTP API.
     *
     * WHY: Serves chain data as JSON so the web-based block explorer can
     * connect and display live blocks, transactions, and network stats.
     *
     * @param port HTTP port to listen on (default 8080).
     * @return URL the explorer should connect to.
     */
    fun startExplorerApi(port: Int = 8080): String {
        return callNode { it.startExplorerApi(port.toUShort()) }
    }

    // ========================================================================
    // Consensus methods
    // ========================================================================

    /**
     * Start the consensus engine and slot timer.
     *
     * Initializes the consensus engine with a demo committee and starts
     * producing blocks every 4 seconds when this node is selected.
     *
     * Requires: wallet created, network started (optional but recommended).
     *
     * @return Current consensus status.
     */
    fun startConsensus(): ConsensusStatus {
        val ffi = callNode { it.startConsensus() }
        return ffi.toBridge()
    }

    /**
     * Stop the consensus engine.
     */
    fun stopConsensus() {
        callNode { it.stopConsensus() }
    }

    /**
     * Get the current consensus status.
     *
     * @return Consensus status with state, slot, height, and block count.
     */
    fun getConsensusStatus(): ConsensusStatus {
        val ffi = callNode { it.getConsensusStatus() }
        return ffi.toBridge()
    }

    /**
     * Request block sync from connected peers.
     *
     * Checks if this node is behind the network and requests missing blocks.
     *
     * @return Current sync state string (e.g., "synced", "syncing 5/10", "behind 3/10").
     */
    fun requestSync(): String {
        return callNode { it.requestSync() }
    }

    // ========================================================================
    // Mesh transport methods (Phase 3 — Bluetooth + Wi-Fi Direct)
    // ========================================================================

    /**
     * Start the mesh transport layer (Bluetooth LE + Wi-Fi Direct).
     *
     * Enables local peer-to-peer communication for offline transaction relay
     * and connectivity in areas without internet.
     */
    fun startMesh() {
        callNode { it.startMesh() }
        Log.i(TAG, "Mesh transport started")
    }

    /**
     * Stop the mesh transport layer.
     */
    fun stopMesh() {
        callNode { it.stopMesh() }
        Log.i(TAG, "Mesh transport stopped")
    }

    /**
     * Get the current mesh transport status.
     *
     * @return Mesh status with Bluetooth/Wi-Fi Direct state and peer counts.
     */
    fun getMeshStatus(): MeshStatus {
        val ffi = callNode { it.getMeshStatus() }
        return MeshStatus(
            enabled = ffi.enabled,
            bluetoothActive = ffi.bluetoothActive,
            wifiDirectActive = ffi.wifiDirectActive,
            meshPeerCount = ffi.meshPeerCount.toInt(),
            bridgePeerCount = ffi.bridgePeerCount.toInt(),
            pendingRelayCount = ffi.pendingRelayCount.toInt(),
        )
    }

    /**
     * Broadcast a transaction via the mesh layer.
     *
     * WHY: Allows transactions to propagate even without internet connectivity.
     * Mesh peers with internet (bridge peers) relay to the wider network.
     *
     * @param txHex Hex-encoded signed transaction.
     * @return Relay ID for tracking.
     */
    fun meshBroadcastTransaction(txHex: String): String {
        return callNode { it.meshBroadcastTransaction(txHex) }
    }

    // ========================================================================
    // Sharding methods (Phase 3 — Geographic sharding)
    // ========================================================================

    /**
     * Get this node's shard assignment and sharding network info.
     *
     * @return Shard info with shard ID, validator counts, and activity status.
     */
    fun getShardInfo(): ShardInfo {
        val ffi = callNode { it.getShardInfo() }
        return ShardInfo(
            shardId = ffi.shardId.toInt(),
            shardCount = ffi.shardCount.toInt(),
            localValidators = ffi.localValidators.toInt(),
            crossShardValidators = ffi.crossShardValidators.toInt(),
            shardHeight = ffi.shardHeight.toLong(),
            isShardingActive = ffi.isShardingActive,
        )
    }

    /**
     * Get the number of cross-shard transactions queued for routing.
     *
     * @return Count of pending cross-shard messages.
     */
    fun getCrossShardQueueSize(): Int {
        return callNode { it.getCrossShardQueueSize().toInt() }
    }

    // ========================================================================
    // ZK proof methods (Phase 3 — Bulletproofs range + Groth16)
    // ========================================================================

    /**
     * Generate a Bulletproofs range proof for a given value.
     *
     * WHY: Range proofs are used in shielded transactions to prove a value
     * is within a valid range without revealing the actual amount.
     *
     * @param value The value to prove is in range.
     * @param bitWidth Number of bits for the range (e.g., 64 for u64).
     * @return Hex-encoded proof bytes.
     */
    fun generateRangeProof(value: Long, bitWidth: Int): String {
        return callNode { it.generateRangeProof(value.toULong(), bitWidth.toUInt()) }
    }

    /**
     * Verify a Groth16 zero-knowledge proof.
     *
     * WHY: Verification is fast (~5-10ms on ARM) compared to proof generation.
     * Every validator verifies proofs for transactions in each block.
     *
     * @param proofHex Hex-encoded proof bytes.
     * @param publicInputsHex Hex-encoded public inputs.
     * @param vkHex Hex-encoded verification key.
     * @return True if the proof is valid.
     */
    fun verifyGroth16Proof(proofHex: String, publicInputsHex: String, vkHex: String): Boolean {
        return callNode { it.verifyGroth16Proof(proofHex, publicInputsHex, vkHex) }
    }

    // ========================================================================
    // VM info methods (Phase 3 — GratiaVM status)
    // ========================================================================

    /**
     * Get GratiaVM runtime information.
     *
     * @return VM info with runtime type, contract count, gas usage, and memory state.
     */
    fun getVmInfo(): VmInfo {
        val ffi = callNode { it.getVmInfo() }
        return VmInfo(
            runtimeType = ffi.runtimeType,
            contractsLoaded = ffi.contractsLoaded.toInt(),
            totalGasUsed = ffi.totalGasUsed.toLong(),
            memoryWired = ffi.memoryWired,
        )
    }

    // ========================================================================
    // Smart Contract methods
    // ========================================================================

    /**
     * Initialize the GratiaVM with built-in demo contracts.
     *
     * @return List of deployed contract addresses.
     */
    fun initVm(): List<String> {
        return callNode { it.initVm() }
    }

    /**
     * Call a smart contract function.
     *
     * @param contractAddress The "grat:..." address of the deployed contract.
     * @param functionName The function to call.
     * @param gasLimit Maximum gas to spend.
     * @return Execution result with success, return value, gas used, events.
     */
    fun callContract(contractAddress: String, functionName: String, gasLimit: Long = 1_000_000): ContractResult {
        val ffi = callNode { it.callContract(contractAddress, functionName, gasLimit.toULong()) }
        return ContractResult(
            success = ffi.success,
            returnValue = ffi.returnValue,
            gasUsed = ffi.gasUsed.toLong(),
            gasRemaining = ffi.gasRemaining.toLong(),
            events = ffi.events,
            error = ffi.error,
        )
    }

    /**
     * List deployed contracts.
     */
    fun listContracts(): List<String> {
        return callNode { it.listContracts() }
    }

    // ========================================================================
    // Governance methods — One Phone, One Vote
    // ========================================================================

    fun submitProposal(title: String, description: String): String {
        return callNode { it.submitProposal(title, description) }
    }

    fun voteOnProposal(proposalIdHex: String, vote: String) {
        callNode { it.voteOnProposal(proposalIdHex, vote) }
    }

    fun getProposals(): List<BridgeProposal> {
        val ffiList = callNode { it.getProposals() }
        return ffiList.map { ffi ->
            BridgeProposal(
                idHex = ffi.idHex,
                title = ffi.title,
                description = ffi.description,
                status = ffi.status,
                votesYes = ffi.votesYes.toLong(),
                votesNo = ffi.votesNo.toLong(),
                votesAbstain = ffi.votesAbstain.toLong(),
                discussionEndMillis = ffi.discussionEndMillis,
                votingEndMillis = ffi.votingEndMillis,
                submittedBy = ffi.submittedBy,
            )
        }
    }

    fun createPoll(question: String, options: List<String>, durationSecs: Long = 604800): String {
        return callNode { it.createPoll(question, options, durationSecs.toULong()) }
    }

    fun voteOnPoll(pollIdHex: String, optionIndex: Int) {
        callNode { it.voteOnPoll(pollIdHex, optionIndex.toUInt()) }
    }

    fun getPolls(): List<BridgePoll> {
        val ffiList = callNode { it.getPolls() }
        return ffiList.map { ffi ->
            BridgePoll(
                idHex = ffi.idHex,
                question = ffi.question,
                options = ffi.options,
                votes = ffi.votes.map { it.toLong() },
                totalVoters = ffi.totalVoters.toLong(),
                endMillis = ffi.endMillis,
                createdBy = ffi.createdBy,
            )
        }
    }

    // ========================================================================
    // Private helpers
    // ========================================================================

    /**
     * Execute a block against the initialized GratiaNode, mapping FFI errors
     * to [GratiaBridgeException].
     */
    private fun <T> callNode(block: (GratiaNode) -> T): T {
        val n = node ?: throw GratiaBridgeException(
            "Rust core not initialized. Call initialize() first."
        )
        return try {
            block(n)
        } catch (e: FfiException) {
            Log.e(TAG, "FFI error: ${e.message}", e)
            throw GratiaBridgeException(e.message ?: "Unknown FFI error", e)
        }
    }
}

// ============================================================================
// Bridge data classes — mirror the FFI types from gratia-ffi/src/lib.rs
// ============================================================================

/**
 * Wallet information for the UI layer.
 * Mirrors [FfiWalletInfo] from the Rust FFI.
 */
data class WalletInfo(
    val address: String,
    val balanceLux: Long,
    val miningState: String,
) {
    /** Balance formatted as GRAT with 6 decimal places. */
    val balanceGrat: String
        get() {
            val whole = balanceLux / 1_000_000
            val fractional = balanceLux % 1_000_000
            return "$whole.${fractional.toString().padStart(6, '0')}"
        }
}

/**
 * Transaction record for the UI layer.
 * Mirrors [FfiTransactionInfo] from the Rust FFI.
 */
data class TransactionInfo(
    val hashHex: String,
    val direction: String,
    val counterparty: String?,
    val amountLux: Long,
    val timestampMillis: Long,
    val status: String,
)

/**
 * Mining status for the UI layer.
 * Mirrors [FfiMiningStatus] from the Rust FFI.
 */
data class MiningStatus(
    val state: String,
    val batteryPercent: Int,
    val isPluggedIn: Boolean,
    val currentDayPolValid: Boolean,
    val presenceScore: Int,
)

/**
 * Proof of Life status for the UI layer.
 * Mirrors [FfiProofOfLifeStatus] from the Rust FFI.
 */
data class ProofOfLifeStatus(
    val isValidToday: Boolean,
    val consecutiveDays: Long,
    val isOnboarded: Boolean,
    val parametersMet: List<String>,
)

/**
 * Staking information for the UI layer.
 * Mirrors [FfiStakeInfo] from the Rust FFI.
 */
data class StakeInfo(
    val nodeStakeLux: Long,
    val overflowAmountLux: Long,
    val totalCommittedLux: Long,
    val stakedAtMillis: Long,
    val meetsMinimum: Boolean,
)

/**
 * Sensor events submitted from Android sensor managers to the Rust PoL engine.
 * Mirrors [FfiSensorEvent] from the Rust FFI.
 */
sealed class SensorEvent(val type: String) {
    /** Phone was unlocked by the user. */
    data object Unlock : SensorEvent("unlock")

    /** A screen interaction session was recorded. */
    data class Interaction(val durationSecs: Int) : SensorEvent("interaction")

    /** Phone orientation changed (picked up, rotated, set down). */
    data object OrientationChange : SensorEvent("orientation_change")

    /** Accelerometer detected human-consistent motion. */
    data object Motion : SensorEvent("motion")

    /** A GPS fix was obtained. */
    data class GpsUpdate(val lat: Float, val lon: Float) : SensorEvent("gps_update")

    /** Wi-Fi scan completed with visible BSSIDs (as opaque hashes). */
    data class WifiScan(val bssidHashes: List<Long>) : SensorEvent("wifi_scan")

    /** Bluetooth scan completed with nearby peers (as opaque hashes). */
    data class BluetoothScan(val peerHashes: List<Long>) : SensorEvent("bluetooth_scan")

    /** Charge state changed (plugged in or unplugged). */
    data class ChargeEvent(val isCharging: Boolean) : SensorEvent("charge_event")

    /**
     * Convert this bridge-layer sensor event to the UniFFI-generated FFI type.
     */
    fun toFfi(): FfiSensorEvent = when (this) {
        is Unlock -> FfiSensorEvent.Unlock
        is Interaction -> FfiSensorEvent.Interaction(durationSecs.toUInt())
        is OrientationChange -> FfiSensorEvent.OrientationChange
        is Motion -> FfiSensorEvent.Motion
        is GpsUpdate -> FfiSensorEvent.GpsUpdate(lat, lon)
        is WifiScan -> FfiSensorEvent.WifiScan(bssidHashes.map { it.toULong() })
        is BluetoothScan -> FfiSensorEvent.BluetoothScan(peerHashes.map { it.toULong() })
        is ChargeEvent -> FfiSensorEvent.ChargeEvent(isCharging)
    }
}

/**
 * Consensus status for the UI layer.
 * Mirrors [FfiConsensusStatus] from the Rust FFI.
 */
data class ConsensusStatus(
    val state: String,
    val currentSlot: Long,
    val currentHeight: Long,
    val isCommitteeMember: Boolean,
    val blocksProduced: Long,
)

/**
 * Network status for the UI layer.
 * Mirrors [FfiNetworkStatus] from the Rust FFI.
 */
data class NetworkStatus(
    val isRunning: Boolean,
    val peerCount: Int,
    val listenAddress: String?,
)

/**
 * Network events delivered from the Rust core to the UI.
 * Mirrors [FfiNetworkEvent] from the Rust FFI.
 */
sealed class NetworkEvent {
    /** A peer connected to this node. */
    data class PeerConnected(val peerId: String) : NetworkEvent()

    /** A peer disconnected from this node. */
    data class PeerDisconnected(val peerId: String) : NetworkEvent()

    /** A block was received from the network. */
    data class BlockReceived(val height: Long, val producer: String) : NetworkEvent()

    /** A transaction was received from the network. */
    data class TransactionReceived(val hashHex: String) : NetworkEvent()
}

/**
 * Result of a smart contract execution.
 */
data class ContractResult(
    val success: Boolean,
    val returnValue: String,
    val gasUsed: Long,
    val gasRemaining: Long,
    val events: List<String>,
    val error: String?,
)

/**
 * Governance proposal for the UI layer.
 */
data class BridgeProposal(
    val idHex: String,
    val title: String,
    val description: String,
    val status: String,
    val votesYes: Long,
    val votesNo: Long,
    val votesAbstain: Long,
    val discussionEndMillis: Long,
    val votingEndMillis: Long,
    val submittedBy: String,
)

/**
 * On-chain poll for the UI layer.
 */
data class BridgePoll(
    val idHex: String,
    val question: String,
    val options: List<String>,
    val votes: List<Long>,
    val totalVoters: Long,
    val endMillis: Long,
    val createdBy: String,
)

/**
 * Mesh transport status for the UI layer.
 * Mirrors [FfiMeshStatus] from the Rust FFI.
 */
data class MeshStatus(
    val enabled: Boolean,
    val bluetoothActive: Boolean,
    val wifiDirectActive: Boolean,
    val meshPeerCount: Int,
    val bridgePeerCount: Int,
    val pendingRelayCount: Int,
)

/**
 * Shard assignment info for the UI layer.
 * Mirrors [FfiShardInfo] from the Rust FFI.
 */
data class ShardInfo(
    val shardId: Int,
    val shardCount: Int,
    val localValidators: Int,
    val crossShardValidators: Int,
    val shardHeight: Long,
    val isShardingActive: Boolean,
)

/**
 * GratiaVM runtime info for the UI layer.
 * Mirrors [FfiVmInfo] from the Rust FFI.
 */
data class VmInfo(
    val runtimeType: String,
    val contractsLoaded: Int,
    val totalGasUsed: Long,
    val memoryWired: Boolean,
)

/**
 * Exception thrown by the bridge layer when an FFI operation fails.
 */
class GratiaBridgeException(message: String, cause: Throwable? = null) :
    RuntimeException(message, cause)

// ============================================================================
// Extension functions for FFI -> Bridge conversion
// ============================================================================

/** Convert [FfiConsensusStatus] to bridge-layer [ConsensusStatus]. */
private fun FfiConsensusStatus.toBridge() = ConsensusStatus(
    state = state,
    currentSlot = currentSlot.toLong(),
    currentHeight = currentHeight.toLong(),
    isCommitteeMember = isCommitteeMember,
    blocksProduced = blocksProduced.toLong(),
)

/** Convert [FfiMiningStatus] to bridge-layer [MiningStatus]. */
private fun FfiMiningStatus.toBridge() = MiningStatus(
    state = state,
    batteryPercent = batteryPercent.toInt(),
    isPluggedIn = isPluggedIn,
    currentDayPolValid = currentDayPolValid,
    presenceScore = presenceScore.toInt(),
)
