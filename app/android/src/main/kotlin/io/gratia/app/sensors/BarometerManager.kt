package io.gratia.app.sensors

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener as AndroidSensorEventListener
import android.hardware.SensorManager
import android.util.Log

/**
 * Barometric pressure sensor manager (optional).
 *
 * Reads atmospheric pressure data from TYPE_PRESSURE at low frequency.
 * This is an optional sensor that contributes +5 to the Composite Presence
 * Score but is NOT required for the PoL threshold. Many mid-range and
 * budget phones lack a barometer.
 *
 * Barometric data feeds into the Oracle layer (Phase 3) for environmental
 * data aggregation and also provides altitude estimation for geographic
 * attestation proofs.
 *
 * WHY optional: The PoL threshold is designed so 50%+ of smartphones
 * worldwide can pass. Barometers are found on ~40% of Android devices,
 * so requiring one would exclude too many phones.
 */
class BarometerManager(
    private val context: Context
) {

    companion object {
        private const val TAG = "GratiaBarometer"

        // WHY: 5-minute sampling is sufficient for environmental data.
        // Atmospheric pressure changes slowly (weather fronts, altitude changes).
        private const val SAMPLE_INTERVAL_MS = 5L * 60 * 1000 // 5 minutes
    }

    private val sensorManager = context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager
    private val barometer = sensorManager?.getDefaultSensor(Sensor.TYPE_PRESSURE)

    private var isRunning = false
    private var lastSampleTimeMs = 0L

    /** Most recent pressure reading in hPa (hectopascals / millibars). */
    @Volatile
    var currentPressureHpa: Float = 0f
        private set

    /** Whether at least one valid reading has been obtained. */
    @Volatile
    var hasReading: Boolean = false
        private set

    /** Optional callback for pressure updates. */
    var onPressureUpdate: ((pressureHpa: Float) -> Unit)? = null

    private val sensorListener = object : AndroidSensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            if (event.sensor.type != Sensor.TYPE_PRESSURE) return

            val now = System.currentTimeMillis()
            // WHY: Debounce at the sampling interval. The hardware sensor may
            // deliver readings at a higher rate than we need.
            if (now - lastSampleTimeMs < SAMPLE_INTERVAL_MS && hasReading) return

            lastSampleTimeMs = now
            currentPressureHpa = event.values[0]
            hasReading = true

            Log.d(TAG, "Pressure reading: ${currentPressureHpa} hPa")
            onPressureUpdate?.invoke(currentPressureHpa)
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {
            // No-op.
        }
    }

    /**
     * Start reading barometric pressure data.
     *
     * Gracefully returns without error if the sensor is not available.
     */
    fun start() {
        if (isRunning) return

        if (sensorManager == null || barometer == null) {
            Log.d(TAG, "Barometer not available on this device (optional sensor)")
            return
        }

        // WHY: SENSOR_DELAY_NORMAL is the lowest power option. Combined with
        // our debounce logic, this gives us one reading per 5 minutes at
        // minimal battery cost.
        sensorManager.registerListener(
            sensorListener,
            barometer,
            SensorManager.SENSOR_DELAY_NORMAL
        )

        isRunning = true
        Log.i(TAG, "Barometer tracking started")
    }

    /** Stop reading and release sensor resources. */
    fun stop() {
        if (!isRunning) return

        sensorManager?.unregisterListener(sensorListener)
        isRunning = false
        hasReading = false
        Log.i(TAG, "Barometer tracking stopped")
    }

    /** Check whether this manager is actively collecting data. */
    fun isActive(): Boolean = isRunning

    /** Check whether the barometer sensor is present on this device. */
    fun isAvailable(): Boolean = barometer != null
}
