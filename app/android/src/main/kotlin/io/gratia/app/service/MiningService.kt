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
    private var networkPollJob: Job? = null

    /** Job for the notification update loop. */
    private var notificationJob: Job? = null

    /** Wake lock to keep CPU active during mining computation. */
    private var wakeLock: PowerManager.WakeLock? = null

    /** WiFi lock to prevent WiFi radio from entering power-save mode.
     * WHY: Samsung's network stack buffers/drops UDP packets when WiFi enters
     * power-save mode, causing QUIC connections to silently die. This lock
     * keeps WiFi in high-performance mode during mining so libp2p connections
     * remain stable for BFT consensus. */
    private var wifiLock: android.net.wifi.WifiManager.WifiLock? = null

    /** Receiver for battery state changes (for immediate unplug detection). */
    private var batteryReceiver: BroadcastReceiver? = null

    /** Receiver for WiFi connectivity changes (restart network on reconnect).
     * WHY: When WiFi drops and reconnects, libp2p's existing sockets are dead.
     * The old QUIC/TCP connections silently fail. mDNS multicast also dies.
     * Without restarting the network layer, the phone can't rediscover peers
     * or reach the bootstrap server after a WiFi toggle. */
    private var connectivityReceiver: BroadcastReceiver? = null

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
            acquire(2 * 60 * 60 * 1000L)
        }

        // Acquire WiFi lock to prevent WiFi power-save mode.
        // WHY: Samsung budget phones (A06) aggressively power-save WiFi when the
        // screen is off, buffering/dropping UDP packets. This kills QUIC connections
        // within 30-60 seconds, breaking BFT consensus. WIFI_MODE_FULL_HIGH_PERF
        // keeps the WiFi radio in full-power mode so libp2p connections stay alive.
        // Battery impact is minimal since mining only runs while plugged in.
        try {
            val wifiManager = applicationContext.getSystemService(Context.WIFI_SERVICE) as android.net.wifi.WifiManager
            @Suppress("DEPRECATION")
            wifiLock = wifiManager.createWifiLock(
                android.net.wifi.WifiManager.WIFI_MODE_FULL_HIGH_PERF,
                "gratia:mining"
            ).apply { acquire() }
            Log.i(TAG, "WiFi lock acquired (high-perf mode)")
        } catch (e: Exception) {
            Log.w(TAG, "Failed to acquire WiFi lock: ${e.message}")
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
        // WHY: If the user stopped mining, don't keep the service alive.
        // START_NOT_STICKY tells Android NOT to restart the service after
        // stopSelf(). Without this, START_STICKY causes Android to recreate
        // the service immediately after stopSelf(), restarting mining.
        if (GratiaCoreManager.userStoppedMining) {
            Log.i(TAG, "MiningService onStartCommand — user stopped, not restarting")
            stopSelf()
            return START_NOT_STICKY
        }

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

        connectivityReceiver?.let {
            try { unregisterReceiver(it) } catch (_: Exception) {}
        }
        connectivityReceiver = null

        wakeLock?.let {
            if (it.isHeld) it.release()
        }
        wakeLock = null

        wifiLock?.let {
            if (it.isHeld) it.release()
            Log.i(TAG, "WiFi lock released")
        }
        wifiLock = null

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

        // WHY: If the user manually stopped mining, don't auto-restart.
        if (GratiaCoreManager.userStoppedMining) {
            Log.i(TAG, "User stopped mining — MiningService not starting")
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

        // WHY: WiFi reconnect detection must start with mining so that
        // if WiFi drops and reconnects, we restart the network layer
        // (recreate libp2p sockets, re-dial bootstrap, rejoin gossipsub).
        registerConnectivityReceiver()

        // Start the power/thermal monitoring loop.
        monitorJob = serviceScope.launch {
            monitorConditions()
        }

        // Start the notification update loop.
        notificationJob = serviceScope.launch {
            updateNotificationLoop()
        }

        // WHY: Poll network events continuously so blocks, transactions,
        // node announcements, and BFT signatures from peers are processed.
        // Without this, the FFI event channel is never drained and the
        // phones can't sync blocks, exchange signatures, or rebuild the
        // committee — even when they're connected via libp2p. Previously
        // this only ran when the user was on the Network screen.
        networkPollJob = serviceScope.launch(Dispatchers.IO) {
            pollNetworkEvents()
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

        networkPollJob?.cancel()
        networkPollJob = null

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
                // WHY: Check consensus state first. Block production and mining
                // rewards only happen inside the slot timer, which exits when
                // consensus stops. Without this check, the MiningService keeps
                // running (showing "Mining" in the UI) but earns nothing.
                try {
                    val consensusStatus = GratiaCoreManager.getConsensusStatus()
                    if (consensusStatus.state == "stopped") {
                        Log.i(TAG, "Consensus stopped — stopping mining service")
                        stopSelf()
                        return
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "Failed to check consensus status: ${e.message}")
                }

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
                    // WHY: User tapped "Stop Mining" — stop the service immediately.
                    // Without this check, the service keeps running and showing
                    // "Mining" in the UI even though the Rust side is stopped.
                    GratiaCoreManager.userStoppedMining -> {
                        Log.i(TAG, "User stopped mining — stopping MiningService")
                        stopSelf()
                        return
                    }
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
                    status.state == "mining" || (status.state == "proof_of_life" && isPluggedIn && batteryPercent >= 80) -> {
                        // WHY: Keep mining when plugged in + above 80% even if
                        // PoL state says "proof_of_life". During genesis / early
                        // network, PoL isn't complete yet but mining is allowed
                        // (zero-delay onboarding). The Rust consensus engine
                        // produces blocks regardless — we just show the notification.
                        updateUiState(status)
                    }
                    else -> {
                        // Mining conditions truly not met (unplugged, battery low).
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

    // -- Network Event Polling --------------------------------------------

    /**
     * Continuously poll the Rust core for network events.
     *
     * WHY: The FFI event channel buffers incoming blocks, transactions,
     * node announcements, and BFT signatures from peers. Without polling,
     * these events pile up and are never processed — meaning the phones
     * can't sync blocks, exchange BFT signatures, or discover each other
     * for committee selection. This must run continuously, not just when
     * the user is on a specific screen.
     */
    private suspend fun pollNetworkEvents() {
        // WHY: 500ms interval — fast enough to process blocks within the
        // 4-second slot time, slow enough to avoid burning CPU on mobile.
        val POLL_INTERVAL_MS = 500L

        while (serviceScope.isActive) {
            try {
                if (GratiaCoreManager.isInitialized) {
                    val events = GratiaCoreManager.pollNetworkEvents()
                    if (events.isNotEmpty()) {
                        Log.d(TAG, "Processed ${events.size} network events")
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "Network poll error: ${e.message}")
            }
            delay(POLL_INTERVAL_MS)
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

    /**
     * Register a receiver for WiFi connectivity changes.
     *
     * WHY: When WiFi drops and reconnects (user toggles WiFi, walks out of
     * range then back), libp2p's existing sockets die silently. The phone
     * can't rediscover peers or reach bootstrap until the network layer is
     * restarted. This receiver detects WiFi reconnection and triggers a
     * stop+start cycle on the network layer, recreating all sockets.
     */
    private fun registerConnectivityReceiver() {
        connectivityReceiver = object : BroadcastReceiver() {
            @Suppress("DEPRECATION")
            override fun onReceive(context: Context, intent: Intent) {
                val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? android.net.ConnectivityManager
                val activeNetwork = cm?.activeNetworkInfo
                val isConnected = activeNetwork?.isConnected == true
                val isWifi = activeNetwork?.type == android.net.ConnectivityManager.TYPE_WIFI

                if (isConnected && isWifi) {
                    Log.i(TAG, "WiFi reconnected — restarting network layer")
                    serviceScope.launch(Dispatchers.IO) {
                        try {
                            // WHY: Stop then start recreates all libp2p sockets,
                            // re-subscribes to gossipsub topics, re-joins mDNS,
                            // and re-dials the bootstrap server. Without this,
                            // dead sockets persist indefinitely after WiFi toggle.
                            io.gratia.app.bridge.GratiaCoreManager.stopNetwork()
                            Thread.sleep(1000) // Let sockets close
                            io.gratia.app.bridge.GratiaCoreManager.startNetwork(listenPort = 9000)
                            Log.i(TAG, "Network layer restarted after WiFi reconnect")

                            // Re-start consensus to rebuild committee with fresh connections
                            try {
                                io.gratia.app.bridge.GratiaCoreManager.startConsensus()
                            } catch (e: Exception) {
                                // Already running — that's fine
                                Log.d(TAG, "Consensus already running: ${e.message}")
                            }
                        } catch (e: Exception) {
                            Log.w(TAG, "Network restart failed: ${e.message}")
                        }
                    }
                }
            }
        }

        @Suppress("DEPRECATION")
        val filter = IntentFilter(android.net.ConnectivityManager.CONNECTIVITY_ACTION)
        registerReceiver(connectivityReceiver, filter)
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

            // WHY: Rewards come ONLY from BFT-finalized blocks, not from
            // per-minute ticks. The old 1 GRAT/minute tick was a Phase 1
            // placeholder that allowed solo phones to earn without consensus.
            // Now we just read the current wallet balance (which is updated
            // by the consensus engine when blocks are BFT-finalized).
            try {
                val currentBalanceLux = GratiaCoreManager.tickMiningReward()
                sessionEarningsLux = currentBalanceLux
                prefs.edit().putLong(PREF_KEY_BALANCE_LUX, currentBalanceLux).apply()
                val elapsedMinutes = (System.currentTimeMillis() - sessionStartTimeMs) / 60_000
                Log.d(TAG, "Balance check: $currentBalanceLux Lux, session=${elapsedMinutes}m")
            } catch (e: Exception) {
                Log.e(TAG, "Failed to check balance: ${e.message}")
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
