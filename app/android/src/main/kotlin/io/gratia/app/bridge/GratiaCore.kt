package io.gratia.app.bridge

import android.util.Log

/**
 * Bridge to the Rust core via UniFFI.
 *
 * This singleton manages the lifecycle of the GratiaNode (the Rust-side entry
 * point) and exposes its methods to the Kotlin UI and service layers.
 *
 * Currently returns mock/placeholder data so the Android UI can be developed
 * independently of the Rust core. When the UniFFI bindings are generated, the
 * placeholder implementations will be replaced with real calls to the
 * auto-generated `uniffi.gratia.GratiaNode` class.
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

    /** Data directory passed to the Rust core for persistent storage. */
    private var dataDir: String? = null

    // TODO: Replace with real UniFFI-generated GratiaNode instance:
    //   private var node: uniffi.gratia.GratiaNode? = null

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

        this.dataDir = dataDir

        // TODO: Load the native library and create the real GratiaNode:
        //   System.loadLibrary("gratia_ffi")
        //   node = uniffi.gratia.GratiaNode(dataDir)

        isInitialized = true
        Log.i(TAG, "GratiaCoreManager initialized (mock mode)")
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
        ensureInitialized()

        // TODO: return node!!.createWallet()

        // Mock: return a deterministic placeholder address
        Log.i(TAG, "createWallet() — returning mock address")
        return "grat:" + "a1b2c3d4e5f6".repeat(5) + "a1b2c3d4"
    }

    /**
     * Get current wallet information.
     *
     * @return Wallet info containing address, balance, and mining state.
     */
    fun getWalletInfo(): WalletInfo {
        ensureInitialized()

        // TODO: val ffi = node!!.getWalletInfo()
        //       return WalletInfo(ffi.address, ffi.balanceLux, ffi.miningState)

        return WalletInfo(
            address = "grat:" + "a1b2c3d4e5f6".repeat(5) + "a1b2c3d4",
            balanceLux = 0L,
            miningState = "proof_of_life",
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
        ensureInitialized()

        // TODO: return node!!.sendTransfer(to, amountLux.toULong())

        Log.i(TAG, "sendTransfer() — mock: to=$to, amount=$amountLux Lux")
        return "mock_tx_" + System.currentTimeMillis().toString(16)
    }

    /**
     * Get the transaction history for this wallet.
     *
     * @return List of transaction records.
     */
    fun getTransactionHistory(): List<TransactionInfo> {
        ensureInitialized()

        // TODO: return node!!.getTransactionHistory().map { TransactionInfo.fromFfi(it) }

        return emptyList()
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
        ensureInitialized()

        // TODO: val ffi = node!!.getMiningStatus()
        //       return MiningStatus.fromFfi(ffi)

        return MiningStatus(
            state = "proof_of_life",
            batteryPercent = 0,
            isPluggedIn = false,
            currentDayPolValid = false,
            presenceScore = 0,
        )
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
        ensureInitialized()

        // TODO: val ffi = node!!.updatePowerState(isPluggedIn, batteryPercent.toUByte())
        //       return MiningStatus.fromFfi(ffi)

        Log.d(TAG, "updatePowerState() — plugged=$isPluggedIn, battery=$batteryPercent%")
        return MiningStatus(
            state = if (isPluggedIn && batteryPercent >= 80) "pending_activation" else "proof_of_life",
            batteryPercent = batteryPercent,
            isPluggedIn = isPluggedIn,
            currentDayPolValid = false,
            presenceScore = 0,
        )
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
        ensureInitialized()

        // TODO: val ffi = node!!.startMining()
        //       return MiningStatus.fromFfi(ffi)

        Log.i(TAG, "startMining() — mock")
        return getMiningStatus()
    }

    /**
     * Stop mining. Reverts to Proof of Life passive collection mode.
     *
     * @return Updated mining status.
     */
    fun stopMining(): MiningStatus {
        ensureInitialized()

        // TODO: val ffi = node!!.stopMining()
        //       return MiningStatus.fromFfi(ffi)

        Log.i(TAG, "stopMining() — mock")
        return getMiningStatus()
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
        ensureInitialized()

        // TODO: val ffi = node!!.getProofOfLifeStatus()
        //       return ProofOfLifeStatus.fromFfi(ffi)

        return ProofOfLifeStatus(
            isValidToday = false,
            consecutiveDays = 0L,
            isOnboarded = false,
            parametersMet = emptyList(),
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
        ensureInitialized()

        // TODO: node!!.submitSensorEvent(event.toFfi())

        Log.d(TAG, "submitSensorEvent() — ${event.type}")
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
        ensureInitialized()

        // TODO: return node!!.finalizeDay()

        Log.i(TAG, "finalizeDay() — mock: returning false")
        return false
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
        ensureInitialized()

        // TODO: return node!!.stake(amountLux.toULong())

        Log.i(TAG, "stake() — mock: $amountLux Lux")
        return "mock_stake_" + System.currentTimeMillis().toString(16)
    }

    /**
     * Unstake GRAT (subject to cooldown period).
     *
     * @param amountLux Amount to unstake in Lux.
     * @return Transaction hash as hex string.
     */
    fun unstake(amountLux: Long): String {
        ensureInitialized()

        // TODO: return node!!.unstake(amountLux.toULong())

        Log.i(TAG, "unstake() — mock: $amountLux Lux")
        return "mock_unstake_" + System.currentTimeMillis().toString(16)
    }

    /**
     * Get current staking information for this node.
     *
     * @return Staking info with effective stake, overflow, and minimum status.
     */
    fun getStakeInfo(): StakeInfo {
        ensureInitialized()

        // TODO: val ffi = node!!.getStakeInfo()
        //       return StakeInfo.fromFfi(ffi)

        return StakeInfo(
            nodeStakeLux = 0L,
            overflowAmountLux = 0L,
            totalCommittedLux = 0L,
            stakedAtMillis = 0L,
            meetsMinimum = false,
        )
    }

    // ========================================================================
    // Private helpers
    // ========================================================================

    private fun ensureInitialized() {
        if (!isInitialized) {
            throw GratiaBridgeException("Rust core not initialized. Call initialize() first.")
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
}

/**
 * Exception thrown by the bridge layer when an FFI operation fails.
 */
class GratiaBridgeException(message: String, cause: Throwable? = null) :
    RuntimeException(message, cause)
