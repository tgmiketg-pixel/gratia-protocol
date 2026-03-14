package io.gratia.app.sensors

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager as AndroidBatteryManager
import android.util.Log

/**
 * Battery and charging state manager for Proof of Life and mining mode.
 *
 * Monitors battery level and charging state via system broadcasts. Emits
 * [SensorEventListener.onChargeEvent] when the phone is plugged in or
 * unplugged (satisfying the PoL charge-cycle parameter). Also provides
 * the current battery level and charging state for mining mode activation
 * (requires plugged in + battery >= 80%).
 *
 * This manager also exposes [batteryPercent] and [isPluggedIn] for the
 * mining service to query directly when evaluating mining eligibility.
 */
class GratiaBatteryManager(
    private val context: Context,
    private val listener: SensorEventListener
) {

    companion object {
        private const val TAG = "GratiaBattery"
    }

    /** Current battery percentage (0-100). Updated on every battery change broadcast. */
    @Volatile
    var batteryPercent: Int = 0
        private set

    /** Whether the phone is currently connected to a power source. */
    @Volatile
    var isPluggedIn: Boolean = false
        private set

    private var isRunning = false
    private var receiverRegistered = false

    /**
     * Callback for external consumers (e.g., MiningService) that need to be
     * notified of power state changes beyond the PoL sensor events.
     */
    var onPowerStateChanged: ((isPluggedIn: Boolean, batteryPercent: Int) -> Unit)? = null

    // WHY: We use two separate receivers. The battery-changed receiver fires
    // frequently (every 1% change) and gives us the current state. The
    // power-connected/disconnected receivers fire only on plug/unplug events
    // which are the actual PoL charge-cycle signals.
    private val batteryChangedReceiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context, intent: Intent) {
            if (intent.action != Intent.ACTION_BATTERY_CHANGED) return
            updateBatteryState(intent)
        }
    }

    private val powerConnectionReceiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context, intent: Intent) {
            when (intent.action) {
                Intent.ACTION_POWER_CONNECTED -> {
                    Log.d(TAG, "Power connected")
                    isPluggedIn = true
                    listener.onChargeEvent(isCharging = true)
                    onPowerStateChanged?.invoke(true, batteryPercent)
                }
                Intent.ACTION_POWER_DISCONNECTED -> {
                    Log.d(TAG, "Power disconnected")
                    isPluggedIn = false
                    listener.onChargeEvent(isCharging = false)
                    onPowerStateChanged?.invoke(false, batteryPercent)
                }
            }
        }
    }

    /**
     * Start monitoring battery and charging state.
     *
     * No special permissions required — battery state broadcasts are available
     * to all apps.
     */
    fun start() {
        if (isRunning) return

        // Register for battery level changes.
        // WHY: ACTION_BATTERY_CHANGED is a sticky broadcast — registerReceiver
        // immediately returns the current battery state, so we get the initial
        // reading without a separate query.
        val batteryFilter = IntentFilter(Intent.ACTION_BATTERY_CHANGED)
        val stickyIntent = context.registerReceiver(batteryChangedReceiver, batteryFilter)
        stickyIntent?.let { updateBatteryState(it) }

        // Register for plug/unplug events.
        val powerFilter = IntentFilter().apply {
            addAction(Intent.ACTION_POWER_CONNECTED)
            addAction(Intent.ACTION_POWER_DISCONNECTED)
        }
        context.registerReceiver(powerConnectionReceiver, powerFilter)

        receiverRegistered = true
        isRunning = true
        Log.i(
            TAG,
            "Battery monitoring started (level=$batteryPercent%, plugged=$isPluggedIn)"
        )
    }

    /** Stop monitoring and unregister receivers. */
    fun stop() {
        if (!isRunning) return

        if (receiverRegistered) {
            try {
                context.unregisterReceiver(batteryChangedReceiver)
            } catch (e: IllegalArgumentException) {
                // Already unregistered.
            }
            try {
                context.unregisterReceiver(powerConnectionReceiver)
            } catch (e: IllegalArgumentException) {
                // Already unregistered.
            }
            receiverRegistered = false
        }

        isRunning = false
        Log.i(TAG, "Battery monitoring stopped")
    }

    /** Check whether this manager is actively monitoring. */
    fun isActive(): Boolean = isRunning

    /**
     * Check whether mining conditions are met right now.
     *
     * Mining requires: plugged in AND battery >= 80%.
     */
    fun isMiningEligible(): Boolean = isPluggedIn && batteryPercent >= 80

    // ========================================================================
    // Internal
    // ========================================================================

    private fun updateBatteryState(intent: Intent) {
        val level = intent.getIntExtra(AndroidBatteryManager.EXTRA_LEVEL, -1)
        val scale = intent.getIntExtra(AndroidBatteryManager.EXTRA_SCALE, -1)

        if (level >= 0 && scale > 0) {
            batteryPercent = (level * 100) / scale
        }

        val plugged = intent.getIntExtra(AndroidBatteryManager.EXTRA_PLUGGED, 0)
        val wasPluggedIn = isPluggedIn
        isPluggedIn = plugged != 0

        // WHY: We detect plug/unplug transitions from the battery-changed
        // broadcast too, as a safety net in case the power-connected broadcast
        // is missed (which can happen on some OEM Android builds).
        if (isPluggedIn != wasPluggedIn) {
            Log.d(TAG, "Charging state changed via battery broadcast: plugged=$isPluggedIn")
            listener.onChargeEvent(isCharging = isPluggedIn)
            onPowerStateChanged?.invoke(isPluggedIn, batteryPercent)
        }

        onPowerStateChanged?.invoke(isPluggedIn, batteryPercent)
    }
}
