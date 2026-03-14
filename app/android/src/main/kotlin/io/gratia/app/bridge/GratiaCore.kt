package io.gratia.app.bridge

import android.util.Log
import uniffi.gratia_ffi.GratiaNode
import uniffi.gratia_ffi.FfiException
import uniffi.gratia_ffi.FfiMiningStatus
import uniffi.gratia_ffi.FfiNetworkEvent
import uniffi.gratia_ffi.FfiNetworkStatus
import uniffi.gratia_ffi.FfiProofOfLifeStatus
import uniffi.gratia_ffi.FfiSensorEvent
import uniffi.gratia_ffi.FfiStakeInfo
import uniffi.gratia_ffi.FfiTransactionInfo
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
 * Exception thrown by the bridge layer when an FFI operation fails.
 */
class GratiaBridgeException(message: String, cause: Throwable? = null) :
    RuntimeException(message, cause)

// ============================================================================
// Extension functions for FFI -> Bridge conversion
// ============================================================================

/** Convert [FfiMiningStatus] to bridge-layer [MiningStatus]. */
private fun FfiMiningStatus.toBridge() = MiningStatus(
    state = state,
    batteryPercent = batteryPercent.toInt(),
    isPluggedIn = isPluggedIn,
    currentDayPolValid = currentDayPolValid,
    presenceScore = presenceScore.toInt(),
)
