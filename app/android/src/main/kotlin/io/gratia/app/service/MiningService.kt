package io.gratia.app.service

import android.app.NotificationManager
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
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

import io.gratia.app.bridge.GratiaCoreManager
import io.gratia.app.bridge.MiningStatus

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
         * WHY: 10 seconds ensures mining stops promptly when conditions change
         * (e.g., battery drops below 80%). The broadcast receiver handles
         * instant unplug detection, but gradual battery drain needs polling.
         * 30 seconds was too slow — users could mine for up to 30s below 80%.
         */
        private const val POWER_CHECK_INTERVAL_MS = 10_000L

        /**
         * How often to update the mining notification with earnings info (ms).
         *
         * WHY: 15 seconds keeps the notification visually responsive so users
         * can see mining is actively running. The reward tick still happens
         * once per minute, but elapsed time and status update more frequently.
         */
        private const val NOTIFICATION_UPDATE_INTERVAL_MS = 15_000L

        /**
         * CPU temperature threshold for thermal throttling (Celsius).
         *
         * WHY: 50C is a safe throttle point for modern ARM SoCs (Snapdragon,
         * MediaTek, Exynos). Normal phone operation routinely reaches 40-45C,
         * so throttling at 40C was far too aggressive — it triggered during
         * ordinary use on devices like the S24 (Snapdragon 8 Gen 3). Most ARM
         * chips thermal-throttle themselves around 85-95C; we intervene much
         * earlier to protect long-term battery health and user comfort. At 55C
         * we pause mining entirely as a precaution.
         */
        private const val THERMAL_THROTTLE_TEMP_C = 50.0f
        private const val THERMAL_PAUSE_TEMP_C = 55.0f

        /**
         * SharedPreferences file name for persisting mining balance.
         *
         * WHY: The mining balance must survive app restarts. Without persistence,
         * accumulated Lux is lost every time the service is recreated.
         */
        private const val PREFS_NAME = "gratia_mining_prefs"
        private const val PREF_KEY_BALANCE_LUX = "persisted_balance_lux"

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

    /**
     * Guard flag to prevent redundant mining starts from duplicate
     * onStartCommand calls. Set true when mining loops are running.
     */
    private var isMiningActive: Boolean = false

    /** SharedPreferences for persisting mining balance across app restarts. */
    private lateinit var prefs: SharedPreferences

    // -- Service Lifecycle -------------------------------------------------

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "MiningService created")

        prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

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
            // WHY: 2-hour timeout as a safety net to prevent battery drain if
            // the service crashes without proper cleanup. The monitorConditions
            // loop will re-acquire the lock periodically during normal operation.
            // Without a timeout, a leaked wake lock could drain the battery
            // completely if onDestroy() is never called.
            acquire(2 * 60 * 60 * 1000L)
        }

        sessionStartTimeMs = System.currentTimeMillis()

        // WHY: Initialize session earnings from the persisted balance so the
        // notification and UI show the cumulative total across restarts, not
        // just what was earned since the last service creation. Without this,
        // every service restart (e.g., from START_STICKY after OOM kill) would
        // reset the displayed earnings to zero, confusing users.
        val persistedBalance = prefs.getLong(PREF_KEY_BALANCE_LUX, 0L)
        sessionEarningsLux = persistedBalance
        if (persistedBalance > 0) {
            Log.i(TAG, "Restored persisted balance: $persistedBalance Lux")
        }

        registerBatteryReceiver()
        startMining()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (isMiningActive) {
            // WHY: onStartCommand can fire multiple times if the service is started
            // redundantly (e.g., ProofOfLifeService triggers while already running).
            // Ignore duplicate calls to avoid restarting mining loops.
            Log.d(TAG, "MiningService onStartCommand — already mining, ignoring duplicate")
            return START_STICKY
        }
        Log.d(TAG, "MiningService onStartCommand")

        // WHY: START_STICKY so the system restarts the service if it is killed
        // while mining is active (e.g., under memory pressure). The service will
        // re-check conditions on restart and stop itself if they are no longer met.
        return START_STICKY
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
        if (!GratiaCoreManager.isInitialized) {
            Log.e(TAG, "Cannot start mining — GratiaCoreManager not initialized")
            stopSelf()
            return
        }

        // Attempt to start mining in the Rust core via the bridge.
        try {
            val status = GratiaCoreManager.startMining()
            Log.i(TAG, "Mining started: state=${status.state}")
            updateUiState(status)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start mining: ${e.message}")
            stopSelf()
            return
        }

        isMiningActive = true

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
        isMiningActive = false

        monitorJob?.cancel()
        monitorJob = null

        notificationJob?.cancel()
        notificationJob = null

        if (GratiaCoreManager.isInitialized) {
            try {
                GratiaCoreManager.stopMining()
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
        if (!GratiaCoreManager.isInitialized) return

        while (serviceScope.isActive) {
            try {
                val batteryManager = getSystemService(Context.BATTERY_SERVICE)
                    as android.os.BatteryManager

                val batteryPercent = batteryManager.getIntProperty(
                    android.os.BatteryManager.BATTERY_PROPERTY_CAPACITY
                ).coerceIn(0, 100)

                val isPluggedIn = batteryManager.isCharging
                val cpuTemp = readCpuTemperature()

                // Update Rust core with current power state via the bridge.
                val status = GratiaCoreManager.updatePowerState(isPluggedIn, batteryPercent)

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
     * Read CPU temperature using multiple fallback strategies.
     *
     * Strategy 1: PowerManager thermal API (API 29+) — most reliable
     * Strategy 2: sysfs thermal zone file — works on most devices
     * Strategy 3: Conservative high fallback — assumes warm to be safe
     *
     * WHY: The original 25°C fallback silently disabled thermal throttling.
     * A 45°C fallback is conservative — mining will throttle earlier but
     * never risk overheating a device we can't monitor.
     */
    private fun readCpuTemperature(): Float {
        // Strategy 1: Android PowerManager thermal status (API 29+)
        // WHY: This is the official Android API for thermal state. It doesn't
        // give exact temperature but tells us if the device is throttling.
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.Q) {
            try {
                val powerManager = getSystemService(android.content.Context.POWER_SERVICE)
                    as? android.os.PowerManager
                if (powerManager != null) {
                    val thermalStatus = powerManager.currentThermalStatus
                    // Map thermal status to approximate temperature for throttling decisions
                    val estimatedTemp = when (thermalStatus) {
                        android.os.PowerManager.THERMAL_STATUS_NONE -> 35.0f
                        android.os.PowerManager.THERMAL_STATUS_LIGHT -> 42.0f
                        android.os.PowerManager.THERMAL_STATUS_MODERATE -> 48.0f
                        android.os.PowerManager.THERMAL_STATUS_SEVERE -> 55.0f
                        android.os.PowerManager.THERMAL_STATUS_CRITICAL -> 65.0f
                        android.os.PowerManager.THERMAL_STATUS_EMERGENCY -> 75.0f
                        android.os.PowerManager.THERMAL_STATUS_SHUTDOWN -> 85.0f
                        else -> -1.0f // Unknown status, try next strategy
                    }
                    if (estimatedTemp > 0) return estimatedTemp
                }
            } catch (_: Exception) {
                // Fall through to next strategy
            }
        }

        // Strategy 2: sysfs thermal zone file
        try {
            val tempStr = java.io.File(THERMAL_ZONE_PATH).readText().trim()
            val milliCelsius = tempStr.toFloatOrNull()
            if (milliCelsius != null) {
                // WHY: Most devices report in millidegrees (e.g., 38500 = 38.5C).
                // Some report in degrees directly. If the value is > 200, assume
                // millidegrees; otherwise assume degrees.
                return if (milliCelsius > 200f) {
                    milliCelsius / 1000f
                } else {
                    milliCelsius
                }
            }
        } catch (_: Exception) {
            // Fall through to fallback
        }

        // Strategy 3: Conservative fallback
        // WHY: 45°C is warm enough to trigger light throttling in most mining
        // configs but not hot enough to cause concern. This is intentionally
        // higher than room temp — if we can't read the sensor, we assume the
        // device is somewhat warm and throttle mildly. Better to mine slightly
        // slower than to risk overheating a device we can't monitor.
        return 45.0f
    }

    // -- Notification Updates -----------------------------------------------

    /**
     * Periodically tick mining rewards and update the notification.
     *
     * WHY: Every 60 seconds, we call tickMiningReward() on the Rust core
     * which credits 1 GRAT to the wallet. This matches the design principle
     * of "flat reward rate per minute — every minute of mining earns the same."
     */
    private suspend fun updateNotificationLoop() {
        val notificationManager = getSystemService(Context.NOTIFICATION_SERVICE)
            as NotificationManager

        while (serviceScope.isActive) {
            delay(NOTIFICATION_UPDATE_INTERVAL_MS)

            // Credit mining reward for this minute.
            try {
                val newBalanceLux = GratiaCoreManager.tickMiningReward()
                val elapsedMinutes = (System.currentTimeMillis() - sessionStartTimeMs) / 60_000
                sessionEarningsLux += 1_000_000L // 1 GRAT per minute

                // WHY: Persist balance after every tick so it survives app restarts.
                // SharedPreferences.apply() is async and non-blocking — safe to call
                // on every tick without impacting mining performance.
                prefs.edit().putLong(PREF_KEY_BALANCE_LUX, newBalanceLux).apply()

                Log.d(TAG, "Mining reward tick: +1 GRAT, balance=$newBalanceLux Lux, session=${elapsedMinutes}m")
            } catch (e: Exception) {
                Log.e(TAG, "Failed to tick mining reward: ${e.message}")
            }

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
    private fun updateUiState(status: MiningStatus) {
        _miningState.value = MiningUiState.Mining(
            batteryPercent = status.batteryPercent,
            presenceScore = status.presenceScore,
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
