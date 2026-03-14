package io.gratia.app.sensors

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener as AndroidSensorEventListener
import android.hardware.SensorManager
import android.util.Log

/**
 * Ambient light sensor manager (optional).
 *
 * Reads ambient light levels from TYPE_LIGHT at low frequency. This is an
 * optional sensor that contributes +3 to the Composite Presence Score but
 * is NOT required for the PoL threshold.
 *
 * Light sensor data provides:
 * 1. Environmental context — indoor/outdoor detection, time-of-day
 *    corroboration (dark at night, bright during the day).
 * 2. Anti-spoofing signal — phone farms in a warehouse will show constant
 *    artificial lighting patterns vs. natural light variation of a phone
 *    carried by a real human throughout the day.
 * 3. Oracle layer (Phase 3) — aggregated light data for environmental
 *    data contracts.
 *
 * WHY optional: While most smartphones have an ambient light sensor (used
 * for auto-brightness), some budget devices or rugged phones may lack one.
 */
class LightSensorManager(
    private val context: Context
) {

    companion object {
        private const val TAG = "GratiaLightSensor"

        // WHY: 10-minute sampling captures the natural arc of daylight and
        // indoor/outdoor transitions without wasting battery. Light level
        // changes are primarily interesting over hours, not seconds.
        private const val SAMPLE_INTERVAL_MS = 10L * 60 * 1000 // 10 minutes
    }

    private val sensorManager = context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager
    private val lightSensor = sensorManager?.getDefaultSensor(Sensor.TYPE_LIGHT)

    private var isRunning = false
    private var lastSampleTimeMs = 0L

    /** Most recent ambient light level in lux. */
    @Volatile
    var currentLux: Float = 0f
        private set

    /** Whether at least one valid reading has been obtained. */
    @Volatile
    var hasReading: Boolean = false
        private set

    /** Optional callback for light level updates. */
    var onLightUpdate: ((lux: Float) -> Unit)? = null

    private val sensorListener = object : AndroidSensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            if (event.sensor.type != Sensor.TYPE_LIGHT) return

            val now = System.currentTimeMillis()
            if (now - lastSampleTimeMs < SAMPLE_INTERVAL_MS && hasReading) return

            lastSampleTimeMs = now
            currentLux = event.values[0]
            hasReading = true

            Log.d(TAG, "Ambient light: ${currentLux} lux")
            onLightUpdate?.invoke(currentLux)
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {
            // No-op — light sensor accuracy changes are not meaningful for PoL.
        }
    }

    /**
     * Start reading ambient light data.
     *
     * Gracefully returns without error if the sensor is not available.
     */
    fun start() {
        if (isRunning) return

        if (sensorManager == null || lightSensor == null) {
            Log.d(TAG, "Light sensor not available on this device (optional sensor)")
            return
        }

        sensorManager.registerListener(
            sensorListener,
            lightSensor,
            SensorManager.SENSOR_DELAY_NORMAL
        )

        isRunning = true
        Log.i(TAG, "Light sensor tracking started")
    }

    /** Stop reading and release sensor resources. */
    fun stop() {
        if (!isRunning) return

        sensorManager?.unregisterListener(sensorListener)
        isRunning = false
        hasReading = false
        Log.i(TAG, "Light sensor tracking stopped")
    }

    /** Check whether this manager is actively collecting data. */
    fun isActive(): Boolean = isRunning

    /** Check whether the ambient light sensor is present on this device. */
    fun isAvailable(): Boolean = lightSensor != null
}
