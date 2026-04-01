package io.gratia.app.service

import android.app.AlarmManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import io.gratia.app.bridge.GratiaCoreManager
import io.gratia.app.bridge.SensorEvent
import io.gratia.app.sensors.SensorEventListener
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.util.Calendar
import java.util.TimeZone

/**
 * Foreground service for passive Proof of Life data collection.
 *
 * This service runs continuously in the background, collecting sensor
 * attestation data from normal phone usage. It has zero noticeable
 * battery impact because:
 * - Sensor managers use low-power / passive modes where available
 * - GPS uses the fused location provider at ~30-minute intervals
 * - Accelerometer and orientation use batched delivery
 * - Wi-Fi and Bluetooth scans piggyback on system scans
 *
 * The service:
 * 1. Starts all sensor managers on creation
 * 2. Receives sensor events via [SensorEventListener]
 * 3. Forwards events to the Rust core via [GratiaNode.submitSensorEvent]
 * 4. Handles day rollover at midnight UTC (calls finalize_day())
 * 5. Monitors battery/charging state for MiningService activation
 *
 * Lifecycle:
 * - Started on boot via [BootReceiver]
 * - Started when the app opens
 * - Runs as a foreground service with a silent persistent notification
 * - Survives app being swiped away (START_STICKY)
 * - Uses WorkManager for guaranteed periodic tasks (day finalization)
 */
class ProofOfLifeService : Service(), SensorEventListener {

    companion object {
        private const val TAG = "GratiaPoLService"
    }

    /** Coroutine scope tied to this service's lifecycle. */
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** Partial wake lock to keep the CPU alive during sensor processing in doze mode. */
    private var wakeLock: PowerManager.WakeLock? = null

    /**
     * Sensor managers — one per hardware sensor.
     *
     * WHY: We hold references so we can start/stop them cleanly. The actual
     * manager classes live in the sensors/ package and are initialized lazily
     * because some sensors may not be available on all devices.
     *
     * These are stored as Any because the sensor manager classes are defined
     * elsewhere and may not all be implemented yet. Each manager is expected
     * to accept a Context and a SensorEventListener in its constructor.
     */
    private val sensorManagers = mutableListOf<Any>()

    /** Receiver for screen-on/off and user-present events (unlock detection). */
    private var screenReceiver: BroadcastReceiver? = null

    /** Receiver for power/charging state changes. */
    private var powerReceiver: BroadcastReceiver? = null

    // -- Service Lifecycle -------------------------------------------------

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "ProofOfLifeService created")

        // Create notification channels before posting any notification.
        NotificationHelper.createChannels(this)

        // WHY: Call startForeground() immediately in onCreate() to avoid ANR.
        // On Android 12+ the system gives only ~10 seconds after
        // startForegroundService() is called before killing the service.
        startForeground(
            NotificationHelper.NOTIFICATION_ID_POL,
            NotificationHelper.buildProofOfLifeNotification(this)
        )

        // WHY: GratiaCoreManager is initialized by GratiaApplication.onCreate()
        // before any service starts. We verify it here as a safety check.
        // We do NOT call stopSelf() here — the system may have restarted us
        // before the Application finished initializing. Instead we retry after
        // a short delay to give Application.onCreate() time to complete.
        if (!GratiaCoreManager.isInitialized) {
            Log.w(TAG, "GratiaCoreManager not yet initialized — waiting for init")
            serviceScope.launch {
                // WHY: Exponential backoff retry (3s, 6s, 12s, 24s) instead of
                // a single 3s attempt. Application.onCreate() may take longer on
                // slow devices (A06). Without retry, sensors never start and the
                // day ends with 0/8 PoL parameters met.
                var delayMs = 3000L
                val maxRetries = 4
                for (attempt in 1..maxRetries) {
                    delay(delayMs)
                    if (GratiaCoreManager.isInitialized) {
                        Log.i(TAG, "GratiaCoreManager initialized after ${attempt * delayMs / 1000}s — resuming PoL setup")
                        initializeSensors()
                        registerScreenReceiver()
                        registerPowerReceiver()
                        startMidnightRolloverLoop()
                        evaluateMiningConditions()
                        return@launch
                    }
                    delayMs *= 2
                }
                Log.e(TAG, "GratiaCoreManager still not initialized after retries — PoL data collection inactive")
            }
            return
        }

        // Acquire a partial wake lock to ensure sensor callbacks are processed
        // even when the screen is off. This is a PARTIAL lock — it does NOT
        // keep the screen on or prevent doze fully, but it keeps the CPU running
        // for brief processing windows.
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(
            PowerManager.PARTIAL_WAKE_LOCK,
            "gratia:proof_of_life"
        ).apply {
            // WHY: 60-minute timeout as a safety net. The lock is re-acquired
            // on each sensor event. This prevents a leaked wake lock from
            // draining the battery if the service crashes without cleanup.
            // 10 minutes was too short — sensor callbacks can be infrequent
            // (e.g., GPS at 30-min intervals) and the lock would expire
            // between events, causing missed data in doze mode.
            acquire(60 * 60 * 1000L)
        }

        // Start sensor collection.
        initializeSensors()
        registerScreenReceiver()
        registerPowerReceiver()

        // WHY: WorkManager heartbeat (PolHeartbeatWorker) is scheduled by
        // GratiaApplication.onCreate() and runs every 15 minutes to restart
        // this service if the OS kills it. No scheduling needed here.

        // Start the midnight rollover coroutine for precise timing.
        startMidnightRolloverLoop()

        // WHY: Evaluate mining conditions immediately on service start.
        // If the phone is already plugged in and charged above 80% when
        // the app launches (common during development — phone connected
        // to USB), we need to detect this and start mining right away
        // rather than waiting for the next BATTERY_CHANGED broadcast.
        evaluateMiningConditions()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null) {
            // WHY: When the system restarts a START_STICKY service after killing it,
            // it delivers onStartCommand with a null intent. We need to detect this
            // and re-initialize sensors in case onCreate() state was lost.
            Log.i(TAG, "PoL service restarted by system (null intent) — re-initializing sensors")
            if (GratiaCoreManager.isInitialized && sensorManagers.isEmpty()) {
                initializeSensors()
                registerScreenReceiver()
                registerPowerReceiver()
                startMidnightRolloverLoop()
                evaluateMiningConditions()
            }
        } else {
            Log.d(TAG, "onStartCommand received")
        }

        // WHY: START_STICKY tells the system to recreate the service if it's
        // killed due to memory pressure. PoL data collection must be continuous;
        // a gap of several hours could invalidate the day.
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? {
        // WHY: This service is not bound — it runs independently. The UI
        // communicates with GratiaNode directly via the singleton reference.
        return null
    }

    override fun onDestroy() {
        Log.i(TAG, "ProofOfLifeService destroyed — cleaning up")

        // Cancel all coroutines.
        serviceScope.cancel()

        // Release wake lock.
        wakeLock?.let {
            if (it.isHeld) it.release()
        }
        wakeLock = null

        // Unregister broadcast receivers.
        screenReceiver?.let {
            try { unregisterReceiver(it) } catch (_: Exception) {}
        }
        screenReceiver = null

        powerReceiver?.let {
            try { unregisterReceiver(it) } catch (_: Exception) {}
        }
        powerReceiver = null

        // Stop sensor managers.
        stopSensors()

        super.onDestroy()
    }

    /**
     * Called when the user swipes the app from the recent apps list.
     *
     * WHY: On some Android devices/OEMs, swiping the app from recents can kill
     * the service even if it's a foreground service. We explicitly do NOT stop
     * the service here — PoL must keep collecting data. As an extra safety net,
     * we schedule a restart alarm so that even if the OEM kills us anyway, the
     * service comes back within 60 seconds.
     */
    override fun onTaskRemoved(rootIntent: Intent?) {
        Log.i(TAG, "App swiped from recents — PoL service continuing")

        // WHY: Some aggressive OEM Android skins (Xiaomi MIUI, Huawei EMUI,
        // Samsung OneUI) kill foreground services when the app is swiped.
        // AlarmManager provides a fallback restart mechanism. We use
        // setExactAndAllowWhileIdle to work even in Doze mode.
        try {
            val restartIntent = Intent(this, ProofOfLifeService::class.java)
            val pendingIntent = PendingIntent.getService(
                this,
                // WHY: Request code 1 is arbitrary but fixed — ensures we update
                // any existing pending restart rather than creating duplicates.
                1,
                restartIntent,
                // WHY: FLAG_UPDATE_CURRENT instead of FLAG_ONE_SHOT so the alarm
                // can be re-scheduled on subsequent swipe-away events. FLAG_ONE_SHOT
                // caused the PendingIntent to be consumed after the first restart,
                // preventing the service from restarting on later kills.
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )

            val alarmManager = getSystemService(Context.ALARM_SERVICE) as AlarmManager
            // WHY: 60-second delay. Short enough to minimize PoL data gaps,
            // long enough to avoid rapid restart loops if something is wrong.
            alarmManager.setExactAndAllowWhileIdle(
                AlarmManager.ELAPSED_REALTIME_WAKEUP,
                SystemClock.elapsedRealtime() + 60_000L,
                pendingIntent
            )
            Log.d(TAG, "Restart alarm scheduled as safety net (60s)")
        } catch (e: Exception) {
            Log.w(TAG, "Could not schedule restart alarm: ${e.message}")
        }

        super.onTaskRemoved(rootIntent)
    }

    // -- Sensor Initialization ---------------------------------------------

    /**
     * Initialize and start all available sensor managers.
     *
     * Each manager is started in a try-catch so that a missing sensor
     * (e.g., barometer on a budget phone) does not prevent the service
     * from collecting data from sensors that ARE available.
     *
     * WHY: Construction and start() are separate steps because some managers
     * need the constructor to succeed (hardware detection) before start()
     * can register listeners. We call start() immediately after construction.
     */
    private fun initializeSensors() {
        Log.i(TAG, "Initializing sensor managers")

        // WHY: Sensor managers are instantiated via reflection-style try/catch
        // rather than a hard dependency list. This ensures the service starts
        // even if some sensor manager classes are not yet implemented during
        // Phase 1 development.

        tryInitSensor("GPS") {
            io.gratia.app.sensors.GpsManager(this, this).also { it.start() }
        }
        tryInitSensor("Accelerometer") {
            io.gratia.app.sensors.AccelerometerManager(this, this).also { it.start() }
        }
        tryInitSensor("Bluetooth") {
            io.gratia.app.sensors.BluetoothManager(this, this).also { it.start() }
        }
        tryInitSensor("WiFi") {
            io.gratia.app.sensors.GratiaWifiManager(this, this).also { it.start() }
        }
        tryInitSensor("Battery") {
            io.gratia.app.sensors.GratiaBatteryManager(this, this).also { it.start() }
        }
        // WHY: BarometerManager, MagnetometerManager, LightSensorManager, and
        // NfcManager are optional sensors that do not implement SensorEventListener
        // callbacks. They take only a Context and manage their own data internally.
        tryInitSensor("Barometer") {
            io.gratia.app.sensors.BarometerManager(this)
        }
        tryInitSensor("Magnetometer") {
            io.gratia.app.sensors.MagnetometerManager(this)
        }
        tryInitSensor("LightSensor") {
            io.gratia.app.sensors.LightSensorManager(this)
        }
        tryInitSensor("NFC") {
            io.gratia.app.sensors.NfcManager(this)
        }

        Log.i(TAG, "Sensor initialization complete: ${sensorManagers.size} managers active")

        // Start the periodic PoL status logging loop.
        startPolStatusLoop()
    }

    /**
     * Attempt to initialize a single sensor manager. If the class is not
     * found or the sensor hardware is unavailable, log a warning and continue.
     */
    private fun tryInitSensor(name: String, factory: () -> Any) {
        try {
            val manager = factory()
            sensorManagers.add(manager)
            Log.i(TAG, "Sensor manager started: $name")
        } catch (e: Exception) {
            // WHY: Not all phones have all sensors. A budget phone from 2018
            // may lack a barometer or magnetometer. The core four (GPS,
            // accelerometer, Wi-Fi/BT, battery) are required; the rest only
            // boost the Presence Score.
            Log.w(TAG, "Sensor manager unavailable: $name — ${e.message}")
        }
    }

    /** Stop all sensor managers. */
    private fun stopSensors() {
        for (manager in sensorManagers) {
            try {
                // WHY: Sensor managers should implement a stop/destroy method.
                // We call it via duck-typing since the interface is not yet
                // standardized across all manager implementations.
                val stopMethod = manager.javaClass.getMethod("stop")
                stopMethod.invoke(manager)
            } catch (_: NoSuchMethodException) {
                // Manager does not have a stop() method — acceptable during
                // Phase 1 when interfaces are still being finalized.
            } catch (e: Exception) {
                Log.w(TAG, "Error stopping sensor manager: ${e.message}")
            }
        }
        sensorManagers.clear()
    }

    // -- Broadcast Receivers -----------------------------------------------

    /**
     * Register a receiver for screen-on / user-present events.
     *
     * WHY: Unlock detection is the primary PoL signal. The protocol requires
     * at least 10 unlocks spread across 6 hours. ACTION_USER_PRESENT fires
     * when the user dismisses the lockscreen, which is the closest Android
     * equivalent to an "unlock event."
     */
    private fun registerScreenReceiver() {
        screenReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                when (intent.action) {
                    Intent.ACTION_USER_PRESENT -> {
                        Log.d(TAG, "User present (unlock detected)")
                        onUnlock()
                    }
                    Intent.ACTION_SCREEN_ON -> {
                        // WHY: Screen-on without unlock (e.g., notification peek)
                        // is not counted as an unlock, but we log it for debugging.
                        Log.v(TAG, "Screen on")
                    }
                }
            }
        }

        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_USER_PRESENT)
            addAction(Intent.ACTION_SCREEN_ON)
        }

        registerReceiver(screenReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
    }

    /**
     * Register a receiver for power/charging state changes.
     *
     * WHY: The PoL system needs to record charge cycle events (plug/unplug),
     * and the MiningService needs to be started/stopped based on power state.
     */
    private fun registerPowerReceiver() {
        powerReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                when (intent.action) {
                    Intent.ACTION_POWER_CONNECTED -> {
                        Log.i(TAG, "Power connected")
                        onChargeEvent(isCharging = true)
                        evaluateMiningConditions()
                    }
                    Intent.ACTION_POWER_DISCONNECTED -> {
                        Log.i(TAG, "Power disconnected")
                        onChargeEvent(isCharging = false)
                        // Stop mining immediately when unplugged.
                        stopMiningService()
                    }
                    Intent.ACTION_BATTERY_CHANGED -> {
                        evaluateMiningConditions()
                    }
                }
            }
        }

        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_POWER_CONNECTED)
            addAction(Intent.ACTION_POWER_DISCONNECTED)
            addAction(Intent.ACTION_BATTERY_CHANGED)
        }

        registerReceiver(powerReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
    }

    // -- SensorEventListener Implementation --------------------------------

    override fun onUnlock() {
        submitEvent(SensorEvent.Unlock)
    }

    override fun onInteraction(durationSecs: Int) {
        submitEvent(SensorEvent.Interaction(durationSecs))
    }

    override fun onOrientationChange() {
        submitEvent(SensorEvent.OrientationChange)
    }

    override fun onMotion() {
        submitEvent(SensorEvent.Motion)
    }

    override fun onGpsUpdate(lat: Float, lon: Float) {
        submitEvent(SensorEvent.GpsUpdate(lat, lon))
    }

    override fun onWifiScan(bssidHashes: List<Long>) {
        submitEvent(SensorEvent.WifiScan(bssidHashes))
    }

    override fun onBluetoothScan(peerHashes: List<Long>) {
        submitEvent(SensorEvent.BluetoothScan(peerHashes))
    }

    override fun onChargeEvent(isCharging: Boolean) {
        submitEvent(SensorEvent.ChargeEvent(isCharging))
    }

    /**
     * Forward a sensor event to the Rust core via [GratiaCoreManager].
     * Runs on the Default dispatcher to avoid blocking the calling thread
     * (often the main thread for broadcast receivers).
     */
    private fun submitEvent(event: SensorEvent) {
        if (!GratiaCoreManager.isInitialized) {
            Log.w(TAG, "GratiaCoreManager not initialized — dropping sensor event: ${event.type}")
            return
        }

        Log.i(TAG, "PoL sensor event: ${event.type}")

        serviceScope.launch {
            try {
                GratiaCoreManager.submitSensorEvent(event)
            } catch (e: Exception) {
                Log.e(TAG, "Failed to submit sensor event [${event.type}]: ${e.message}")
            }
        }
    }

    // -- Periodic PoL Status Logging ------------------------------------------

    /**
     * Periodically log the current Proof of Life parameter completion status.
     *
     * WHY: Without periodic status logging, the service produces zero visible
     * output even when it's working correctly. This loop logs a summary every
     * 5 minutes so developers can verify data collection is progressing.
     */
    private fun startPolStatusLoop() {
        // WHY: 5-minute interval balances visibility against log spam.
        // Frequent enough to confirm the service is alive and collecting,
        // infrequent enough to not overwhelm logcat.
        val statusIntervalMs = 5L * 60 * 1000 // 5 minutes

        serviceScope.launch {
            // WHY: Initial 60-second delay lets sensor managers finish their
            // first collection cycle before we query status. GPS, Wi-Fi, and
            // Bluetooth all need time for their first scan to complete.
            delay(60_000L)

            while (true) {
                try {
                    logPolStatus()
                } catch (e: Exception) {
                    Log.w(TAG, "Error logging PoL status: ${e.message}")
                }
                delay(statusIntervalMs)
            }
        }
    }

    /**
     * Query the Rust core for current PoL status and log a human-readable summary.
     */
    private fun logPolStatus() {
        if (!GratiaCoreManager.isInitialized) return

        try {
            val status = GratiaCoreManager.getProofOfLifeStatus()
            val metCount = status.parametersMet.size
            // WHY: 8 is the total number of daily PoL parameters defined in the protocol.
            val totalParams = 8
            val metList = if (status.parametersMet.isNotEmpty()) {
                status.parametersMet.joinToString(", ")
            } else {
                "none"
            }

            Log.i(
                TAG,
                "PoL status: $metCount/$totalParams parameters met " +
                    "[valid=${status.isValidToday}, onboarded=${status.isOnboarded}, " +
                    "streak=${status.consecutiveDays}d] — met: $metList"
            )
        } catch (e: Exception) {
            Log.w(TAG, "Could not query PoL status: ${e.message}")
        }
    }

    // -- Mining Condition Evaluation ----------------------------------------

    /**
     * Check whether mining conditions are met and start/stop MiningService.
     *
     * Called whenever power state changes. The actual mining-eligibility logic
     * lives in the Rust core ([GratiaNode.updatePowerState]); we just read
     * the result and manage the Android service accordingly.
     */
    private fun evaluateMiningConditions() {
        if (!GratiaCoreManager.isInitialized) return

        serviceScope.launch {
            try {
                val batteryManager = getSystemService(Context.BATTERY_SERVICE)
                    as android.os.BatteryManager
                val batteryPercent = batteryManager.getIntProperty(
                    android.os.BatteryManager.BATTERY_PROPERTY_CAPACITY
                ).coerceIn(0, 100)
                val isPluggedIn = batteryManager.isCharging

                val status = GratiaCoreManager.updatePowerState(isPluggedIn, batteryPercent)

                // WHY: If the user tapped "Stop Mining", don't auto-restart
                // the MiningService regardless of power state. The flag is
                // cleared when the user taps "Start Mining" again.
                if (GratiaCoreManager.userStoppedMining) {
                    Log.d(TAG, "User stopped mining — skipping auto-start")
                } else {
                    when (status.state) {
                        "mining" -> startMiningService()
                        "battery_low" -> stopMiningService()
                        "proof_of_life" -> {
                            // WHY: During genesis / early network, PoL may not be
                            // complete yet but mining is allowed (zero-delay onboarding).
                            // If plugged in + above 80%, keep MiningService running.
                            // The Rust consensus engine is producing blocks regardless;
                            // the Android service just shows the notification.
                            if (isPluggedIn && batteryPercent >= 80) {
                                Log.d(TAG, "PoL incomplete but plugged in + charged — keeping MiningService")
                                startMiningService()
                            } else {
                                stopMiningService()
                            }
                        }
                        "throttled" -> {
                            // WHY: Keep MiningService running but in throttled state.
                            // The MiningService handles throttle internally.
                            Log.d(TAG, "Mining throttled due to thermal conditions")
                        }
                        "pending_activation" -> {
                            // WHY: Same as proof_of_life — keep mining if conditions met.
                            if (isPluggedIn && batteryPercent >= 80) {
                                startMiningService()
                            }
                        }
                    }
                }
            } catch (e: Exception) {
                Log.e(TAG, "Error evaluating mining conditions: ${e.message}")
            }
        }
    }

    private fun startMiningService() {
        Log.i(TAG, "Starting MiningService")
        val intent = Intent(this, MiningService::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(intent)
        } else {
            startService(intent)
        }
    }

    private fun stopMiningService() {
        Log.i(TAG, "Stopping MiningService")
        val intent = Intent(this, MiningService::class.java)
        stopService(intent)
    }

    // -- Day Rollover / Finalization ----------------------------------------

    /**
     * Coroutine that sleeps until midnight UTC and then calls finalize_day().
     *
     * WHY: WorkManager provides guaranteed execution but with imprecise timing
     * (can be delayed by up to 15 minutes in doze). This coroutine provides
     * precise midnight execution when the service is running. WorkManager
     * serves as a backup for when the service has been killed.
     */
    private fun startMidnightRolloverLoop() {
        serviceScope.launch {
            while (true) {
                // WHY: Recalculate delay on EACH iteration instead of once before
                // the sleep. If the coroutine resumes late (e.g., doze mode delayed
                // wakeup), using a stale pre-calculated delay would cause the next
                // finalization to fire at the wrong time or immediately.
                val now = System.currentTimeMillis()
                val calendar = Calendar.getInstance(TimeZone.getTimeZone("UTC")).apply {
                    timeInMillis = now
                    add(Calendar.DAY_OF_YEAR, 1)
                    set(Calendar.HOUR_OF_DAY, 0)
                    set(Calendar.MINUTE, 0)
                    set(Calendar.SECOND, 0)
                    set(Calendar.MILLISECOND, 0)
                }
                val millisUntilMidnight = calendar.timeInMillis - now

                Log.d(TAG, "Next day finalization in ${millisUntilMidnight / 1000}s")
                delay(millisUntilMidnight)

                performDayFinalization()
            }
        }
    }

    /**
     * Finalize the current day's Proof of Life via the Rust core.
     *
     * Resets the sensor buffer for the new day. Logs whether the day was
     * valid or invalid.
     */
    private fun performDayFinalization() {
        if (!GratiaCoreManager.isInitialized) {
            Log.e(TAG, "Cannot finalize day — GratiaCoreManager not initialized")
            return
        }

        try {
            val isValid = GratiaCoreManager.finalizeDay()
            if (isValid) {
                Log.i(TAG, "Day finalized: VALID")
            } else {
                Log.w(TAG, "Day finalized: INVALID — some PoL parameters were not met")
            }
        } catch (e: Exception) {
            Log.e(TAG, "Error finalizing day: ${e.message}")
        }
    }

    // -- WorkManager Scheduling ------------------------------------------------
    // PolHeartbeatWorker (scheduled by GratiaApplication) runs every 15 minutes
    // and restarts this service if the OS has killed it. DayFinalizationWorker
    // is a future addition for guaranteed midnight rollover when this service
    // is dead — the coroutine-based loop above handles it while the service
    // is alive.
}
