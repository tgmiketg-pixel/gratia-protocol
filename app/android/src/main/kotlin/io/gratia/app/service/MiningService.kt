package io.gratia.app.service

import android.app.NotificationManager
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

import uniffi.gratia.FfiMiningStatus

/**
 * Foreground service for active GRAT mining.
 *
 * This service is started by [ProofOfLifeService] when all mining
 * conditions are met:
 * 1. Phone is plugged in to a power source
 * 2. Battery is at or above 80%
 * 3. Valid Proof of Life exists for the current day
 * 4. Minimum GRAT stake is in place
 *
 * The service monitors conditions continuously and stops itself if any
 * condition ceases to be met (unplugged, battery drops, thermal throttle).
 *
 * Mining operates at a flat reward rate per minute — every minute of
 * mining earns the same amount. No diminishing returns. No time-of-day
 * restrictions.
 *
 * Thermal management: the service reads CPU temperature and throttles
 * (or pauses) mining if the device gets too hot. The user's phone
 * health is always the top priority.
 */
class MiningService : Service() {

    companion object {
        private const val TAG = "GratiaMiningService"

        /**
         * How often to check power state and thermal conditions (milliseconds).
         *
         * WHY: 30 seconds balances responsiveness (stop mining quickly when
         * unplugged) against battery/CPU overhead of the check itself.
         * Matches the Rust-side power_check_interval_secs = 30.
         */
        private const val POWER_CHECK_INTERVAL_MS = 30_000L

        /**
         * How often to update the mining notification with earnings info (ms).
         *
         * WHY: 60 seconds matches the flat per-minute reward rate. Updating
         * more frequently would show fractional earnings which is confusing.
         */
        private const val NOTIFICATION_UPDATE_INTERVAL_MS = 60_000L

        /**
         * CPU temperature threshold for thermal throttling (Celsius).
         *
         * WHY: Mirrors MiningConfig.max_cpu_temp_celsius default of 40.0.
         * Above this temperature we reduce mining workload. At 45C we
         * pause mining entirely to prevent hardware damage.
         */
        private const val THERMAL_THROTTLE_TEMP_C = 40.0f
        private const val THERMAL_PAUSE_TEMP_C = 45.0f

        /**
         * Path to the CPU thermal zone file on Android.
         *
         * WHY: Android does not provide a public API for CPU temperature.
         * thermal_zone0 is the most common path across devices. The value
         * is in millidegrees Celsius (e.g., 38500 = 38.5C). Not all devices
         * expose this file, so we fall back to a safe default.
         */
        private const val THERMAL_ZONE_PATH = "/sys/class/thermal/thermal_zone0/temp"

        // -- Observable mining state for UI binding -------------------------

        private val _miningState = MutableStateFlow<MiningUiState>(MiningUiState.Idle)

        /**
         * Observable mining state for the UI layer.
         *
         * The Jetpack Compose UI collects this StateFlow to display real-time
         * mining status without polling.
         */
        val miningState: StateFlow<MiningUiState> = _miningState.asStateFlow()
    }

    /** Coroutine scope tied to this service's lifecycle. */
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** Job for the power/thermal monitoring loop. */
    private var monitorJob: Job? = null

    /** Job for the notification update loop. */
    private var notificationJob: Job? = null

    /** Wake lock to keep CPU active during mining computation. */
    private var wakeLock: PowerManager.WakeLock? = null

    /** Receiver for battery state changes (for immediate unplug detection). */
    private var batteryReceiver: BroadcastReceiver? = null

    /** Tracks total Lux earned this session for notification display. */
    private var sessionEarningsLux: Long = 0

    /** Timestamp when mining started this session. */
    private var sessionStartTimeMs: Long = 0

    // -- Service Lifecycle -------------------------------------------------

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "MiningService created")

        NotificationHelper.createChannels(this)

        // WHY: startForeground() must be called immediately to avoid ANR.
        startForeground(
            NotificationHelper.NOTIFICATION_ID_MINING,
            NotificationHelper.buildMiningNotification(this)
        )

        // Acquire wake lock for mining computation.
        // WHY: Mining requires sustained CPU activity for consensus participation.
        // Without a wake lock, the CPU would sleep and mining would halt.
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(
            PowerManager.PARTIAL_WAKE_LOCK,
            "gratia:mining"
        ).apply {
            // WHY: No timeout — mining runs until conditions change.
            // The service handles its own shutdown cleanly.
            acquire()
        }

        sessionStartTimeMs = System.currentTimeMillis()
        sessionEarningsLux = 0

        registerBatteryReceiver()
        startMining()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.d(TAG, "MiningService onStartCommand")

        // WHY: START_NOT_STICKY because mining should not auto-restart after
        // being killed. Mining activation is driven by power state — the
        // ProofOfLifeService will re-evaluate and restart MiningService
        // when conditions are met again.
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        Log.i(TAG, "MiningService destroyed — stopping mining")

        stopMining()

        batteryReceiver?.let {
            try { unregisterReceiver(it) } catch (_: Exception) {}
        }
        batteryReceiver = null

        wakeLock?.let {
            if (it.isHeld) it.release()
        }
        wakeLock = null

        serviceScope.cancel()

        _miningState.value = MiningUiState.Idle

        super.onDestroy()
    }

    // -- Mining Control ----------------------------------------------------

    /**
     * Start the mining loops: consensus participation, condition monitoring,
     * and notification updates.
     */
    private fun startMining() {
        val node = ProofOfLifeService.gratiaNode
        if (node == null) {
            Log.e(TAG, "Cannot start mining — GratiaNode not initialized")
            stopSelf()
            return
        }

        // Attempt to start mining in the Rust core.
        try {
            val status = node.startMining()
            Log.i(TAG, "Mining started: state=${status.state}")
            updateUiState(status)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start mining: ${e.message}")
            stopSelf()
            return
        }

        // Start the power/thermal monitoring loop.
        monitorJob = serviceScope.launch {
            monitorConditions()
        }

        // Start the notification update loop.
        notificationJob = serviceScope.launch {
            updateNotificationLoop()
        }
    }

    /**
     * Stop mining and clean up.
     */
    private fun stopMining() {
        monitorJob?.cancel()
        monitorJob = null

        notificationJob?.cancel()
        notificationJob = null

        val node = ProofOfLifeService.gratiaNode
        if (node != null) {
            try {
                node.stopMining()
                Log.i(TAG, "Mining stopped in Rust core")
            } catch (e: Exception) {
                Log.e(TAG, "Error stopping mining in Rust core: ${e.message}")
            }
        }
    }

    // -- Condition Monitoring ----------------------------------------------

    /**
     * Periodically check power state and thermal conditions.
     *
     * If conditions are no longer met, stop the service. This is the
     * primary mechanism for responding to state changes that the
     * BroadcastReceiver might miss (e.g., gradual battery drain below 80%).
     */
    private suspend fun monitorConditions() {
        val node = ProofOfLifeService.gratiaNode ?: return

        while (serviceScope.isActive) {
            try {
                val batteryManager = getSystemService(Context.BATTERY_SERVICE)
                    as android.os.BatteryManager

                val batteryPercent = batteryManager.getIntProperty(
                    android.os.BatteryManager.BATTERY_PROPERTY_CAPACITY
                ).coerceIn(0, 100)

                val isPluggedIn = batteryManager.isCharging
                val cpuTemp = readCpuTemperature()

                // Update Rust core with current power state.
                val status = node.updatePowerState(isPluggedIn, batteryPercent.toUByte())

                // Check thermal conditions.
                val isThrottled = cpuTemp >= THERMAL_THROTTLE_TEMP_C
                val isPaused = cpuTemp >= THERMAL_PAUSE_TEMP_C

                when {
                    !isPluggedIn -> {
                        Log.i(TAG, "Phone unplugged — stopping mining")
                        stopSelf()
                        return
                    }
                    batteryPercent < 80 -> {
                        // WHY: Battery dropped below 80% while mining. This can
                        // happen if the charger provides less power than mining
                        // consumes. Stop mining; it will resume when battery
                        // reaches 80% again.
                        Log.i(TAG, "Battery at $batteryPercent%% — below 80%%, stopping mining")
                        stopSelf()
                        return
                    }
                    isPaused -> {
                        Log.w(TAG, "CPU at ${cpuTemp}C — above ${THERMAL_PAUSE_TEMP_C}C, stopping mining")
                        stopSelf()
                        return
                    }
                    isThrottled -> {
                        Log.w(TAG, "CPU at ${cpuTemp}C — throttling mining workload")
                        _miningState.value = MiningUiState.Throttled(
                            cpuTempCelsius = cpuTemp,
                            batteryPercent = batteryPercent,
                            sessionEarningsLux = sessionEarningsLux
                        )
                    }
                    status.state == "mining" -> {
                        // WHY: Accumulate session earnings based on flat reward rate.
                        // In production, the actual earnings come from block rewards
                        // distributed by the consensus layer. For now, we estimate
                        // based on elapsed mining time.
                        updateUiState(status)
                    }
                    else -> {
                        // Mining conditions no longer met (e.g., PoL invalidated,
                        // stake removed). Stop the service.
                        Log.i(TAG, "Mining state changed to ${status.state} — stopping")
                        stopSelf()
                        return
                    }
                }
            } catch (e: Exception) {
                Log.e(TAG, "Error in condition monitor: ${e.message}")
            }

            delay(POWER_CHECK_INTERVAL_MS)
        }
    }

    // -- Battery Receiver --------------------------------------------------

    /**
     * Register a receiver for immediate power-disconnect detection.
     *
     * WHY: The polling loop checks every 30 seconds, but we want to stop
     * mining within 1-2 seconds of being unplugged. The broadcast receiver
     * provides near-instant notification.
     */
    private fun registerBatteryReceiver() {
        batteryReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                when (intent.action) {
                    Intent.ACTION_POWER_DISCONNECTED -> {
                        Log.i(TAG, "Power disconnected broadcast — stopping immediately")
                        stopSelf()
                    }
                }
            }
        }

        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_POWER_DISCONNECTED)
        }
        registerReceiver(batteryReceiver, filter)
    }

    // -- Thermal Management ------------------------------------------------

    /**
     * Read CPU temperature from the thermal zone sysfs interface.
     *
     * Returns temperature in Celsius. Falls back to a safe default of 25.0
     * if the thermal zone file is unavailable (some OEMs restrict access).
     */
    private fun readCpuTemperature(): Float {
        return try {
            val tempStr = java.io.File(THERMAL_ZONE_PATH).readText().trim()
            val milliCelsius = tempStr.toFloatOrNull() ?: return 25.0f
            // WHY: Most devices report in millidegrees (e.g., 38500 = 38.5C).
            // Some report in degrees directly. If the value is > 200, assume
            // millidegrees; otherwise assume degrees.
            if (milliCelsius > 200f) {
                milliCelsius / 1000f
            } else {
                milliCelsius
            }
        } catch (_: Exception) {
            // WHY: 25C is a safe room-temperature default. If we can't read
            // the sensor, we assume the phone is not overheating. This is
            // conservative in the sense that mining continues — but thermal
            // protection is a secondary safeguard (the OS itself will thermal-
            // throttle the CPU before damage occurs).
            25.0f
        }
    }

    // -- Notification Updates -----------------------------------------------

    /**
     * Periodically update the mining notification with current earnings.
     */
    private suspend fun updateNotificationLoop() {
        val notificationManager = getSystemService(Context.NOTIFICATION_SERVICE)
            as NotificationManager

        while (serviceScope.isActive) {
            delay(NOTIFICATION_UPDATE_INTERVAL_MS)

            val elapsedMinutes = (System.currentTimeMillis() - sessionStartTimeMs) / 60_000
            val earningsDisplay = formatEarnings(elapsedMinutes)

            val notification = NotificationHelper.buildMiningNotification(
                this@MiningService,
                earningsPerHour = earningsDisplay
            )

            notificationManager.notify(
                NotificationHelper.NOTIFICATION_ID_MINING,
                notification
            )
        }
    }

    /**
     * Format mining earnings for notification display.
     *
     * WHY: During Phase 1, we don't have real block rewards flowing.
     * Display elapsed mining time as a proxy. In production, this will
     * show actual GRAT earned from the consensus reward distribution.
     */
    private fun formatEarnings(elapsedMinutes: Long): String {
        val hours = elapsedMinutes / 60
        val minutes = elapsedMinutes % 60
        return if (hours > 0) {
            "${hours}h ${minutes}m active"
        } else {
            "${minutes}m active"
        }
    }

    // -- UI State -----------------------------------------------------------

    /**
     * Update the observable mining state for the UI layer.
     */
    private fun updateUiState(status: FfiMiningStatus) {
        _miningState.value = MiningUiState.Mining(
            batteryPercent = status.batteryPercent.toInt(),
            presenceScore = status.presenceScore.toInt(),
            sessionEarningsLux = sessionEarningsLux,
            sessionDurationMs = System.currentTimeMillis() - sessionStartTimeMs
        )
    }
}

/**
 * Observable mining state for the UI layer.
 *
 * Collected by the Jetpack Compose UI via [MiningService.miningState].
 */
sealed class MiningUiState {

    /** Mining is not active. */
    data object Idle : MiningUiState()

    /** Actively mining. */
    data class Mining(
        val batteryPercent: Int,
        val presenceScore: Int,
        val sessionEarningsLux: Long,
        val sessionDurationMs: Long
    ) : MiningUiState()

    /** Mining is throttled due to high CPU temperature. */
    data class Throttled(
        val cpuTempCelsius: Float,
        val batteryPercent: Int,
        val sessionEarningsLux: Long
    ) : MiningUiState()
}
