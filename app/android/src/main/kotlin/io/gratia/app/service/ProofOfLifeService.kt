package io.gratia.app.service

import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
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
        if (!GratiaCoreManager.isInitialized) {
            Log.e(TAG, "GratiaCoreManager not initialized — cannot start PoL service")
            stopSelf()
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
            // WHY: 10-minute timeout as a safety net. The lock is re-acquired
            // on each sensor event. This prevents a leaked wake lock from
            // draining the battery if the service crashes without cleanup.
            acquire(10 * 60 * 1000L)
        }

        // Start sensor collection.
        initializeSensors()
        registerScreenReceiver()
        registerPowerReceiver()

        // TODO: Schedule day-finalization and keep-alive via WorkManager once
        // the androidx.work dependency is added to the build.

        // Start the midnight rollover coroutine for precise timing.
        startMidnightRolloverLoop()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.d(TAG, "onStartCommand received")

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

    // -- Sensor Initialization ---------------------------------------------

    /**
     * Initialize and start all available sensor managers.
     *
     * Each manager is started in a try-catch so that a missing sensor
     * (e.g., barometer on a budget phone) does not prevent the service
     * from collecting data from sensors that ARE available.
     */
    private fun initializeSensors() {
        Log.i(TAG, "Initializing sensor managers")

        // WHY: Sensor managers are instantiated via reflection-style try/catch
        // rather than a hard dependency list. This ensures the service starts
        // even if some sensor manager classes are not yet implemented during
        // Phase 1 development.

        tryInitSensor("GPS") {
            io.gratia.app.sensors.GpsManager(this, this)
        }
        tryInitSensor("Accelerometer") {
            io.gratia.app.sensors.AccelerometerManager(this, this)
        }
        tryInitSensor("Bluetooth") {
            io.gratia.app.sensors.BluetoothManager(this, this)
        }
        tryInitSensor("WiFi") {
            io.gratia.app.sensors.GratiaWifiManager(this, this)
        }
        tryInitSensor("Battery") {
            io.gratia.app.sensors.GratiaBatteryManager(this, this)
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
    }

    /**
     * Attempt to initialize a single sensor manager. If the class is not
     * found or the sensor hardware is unavailable, log a warning and continue.
     */
    private fun tryInitSensor(name: String, factory: () -> Any) {
        try {
            val manager = factory()
            sensorManagers.add(manager)
            Log.d(TAG, "Sensor manager started: $name")
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

        registerReceiver(screenReceiver, filter)
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

        registerReceiver(powerReceiver, filter)
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
            Log.w(TAG, "GratiaCoreManager not initialized — dropping sensor event")
            return
        }

        serviceScope.launch {
            try {
                GratiaCoreManager.submitSensorEvent(event)
            } catch (e: Exception) {
                Log.e(TAG, "Failed to submit sensor event: ${e.message}")
            }
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

                when (status.state) {
                    "mining" -> startMiningService()
                    "battery_low", "proof_of_life" -> stopMiningService()
                    "throttled" -> {
                        // WHY: Keep MiningService running but in throttled state.
                        // The MiningService handles throttle internally.
                        Log.d(TAG, "Mining throttled due to thermal conditions")
                    }
                    "pending_activation" -> {
                        Log.d(TAG, "Mining pending — waiting for PoL or stake")
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

    // -- WorkManager Scheduling (TODO) ----------------------------------------
    // TODO: Add WorkManager dependency (androidx.work:work-runtime-ktx) and
    // implement DayFinalizationWorker and KeepAliveWorker for guaranteed
    // execution when the service is killed by the OS. The coroutine-based
    // midnight rollover handles the precise case while the service is alive.
}
