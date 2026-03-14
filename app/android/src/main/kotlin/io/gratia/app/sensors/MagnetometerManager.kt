package io.gratia.app.sensors

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener as AndroidSensorEventListener
import android.hardware.SensorManager
import android.util.Log
import kotlin.math.sqrt

/**
 * Magnetometer (magnetic field sensor) manager (optional).
 *
 * Reads geomagnetic field data from TYPE_MAGNETIC_FIELD at low frequency.
 * This is an optional sensor that contributes +4 to the Composite Presence
 * Score but is NOT required for the PoL threshold.
 *
 * Magnetometer data serves two purposes:
 * 1. Environmental fingerprinting — indoor magnetic fields vary by location,
 *    providing weak but useful location corroboration.
 * 2. Oracle layer (Phase 3) — aggregated magnetic field data can detect
 *    geomagnetic anomalies.
 *
 * WHY optional: While most post-2015 smartphones include a magnetometer,
 * some budget devices omit it. The PoL threshold cannot depend on it.
 */
class MagnetometerManager(
    private val context: Context
) {

    companion object {
        private const val TAG = "GratiaMagnetometer"

        // WHY: 5-minute sampling is sufficient. Earth's magnetic field is
        // quasi-static, and indoor magnetic anomalies change only when the
        // phone moves to a different location.
        private const val SAMPLE_INTERVAL_MS = 5L * 60 * 1000 // 5 minutes
    }

    private val sensorManager = context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager
    private val magnetometer = sensorManager?.getDefaultSensor(Sensor.TYPE_MAGNETIC_FIELD)

    private var isRunning = false
    private var lastSampleTimeMs = 0L

    /** Most recent magnetic field magnitude in microtesla. */
    @Volatile
    var currentMagnitudeMicroTesla: Float = 0f
        private set

    /** Most recent raw X/Y/Z components in microtesla. */
    @Volatile
    var currentX: Float = 0f
        private set

    @Volatile
    var currentY: Float = 0f
        private set

    @Volatile
    var currentZ: Float = 0f
        private set

    /** Whether at least one valid reading has been obtained. */
    @Volatile
    var hasReading: Boolean = false
        private set

    /** Optional callback for magnetic field updates. */
    var onMagneticFieldUpdate: ((magnitude: Float, x: Float, y: Float, z: Float) -> Unit)? = null

    private val sensorListener = object : AndroidSensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            if (event.sensor.type != Sensor.TYPE_MAGNETIC_FIELD) return

            val now = System.currentTimeMillis()
            if (now - lastSampleTimeMs < SAMPLE_INTERVAL_MS && hasReading) return

            lastSampleTimeMs = now
            currentX = event.values[0]
            currentY = event.values[1]
            currentZ = event.values[2]
            currentMagnitudeMicroTesla = sqrt(
                currentX * currentX + currentY * currentY + currentZ * currentZ
            )
            hasReading = true

            Log.d(TAG, "Magnetic field: magnitude=${currentMagnitudeMicroTesla}uT " +
                "(x=$currentX, y=$currentY, z=$currentZ)")
            onMagneticFieldUpdate?.invoke(currentMagnitudeMicroTesla, currentX, currentY, currentZ)
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {
            when (accuracy) {
                SensorManager.SENSOR_STATUS_UNRELIABLE ->
                    Log.d(TAG, "Magnetometer accuracy: UNRELIABLE (needs calibration)")
                SensorManager.SENSOR_STATUS_ACCURACY_LOW ->
                    Log.d(TAG, "Magnetometer accuracy: LOW")
                SensorManager.SENSOR_STATUS_ACCURACY_MEDIUM ->
                    Log.d(TAG, "Magnetometer accuracy: MEDIUM")
                SensorManager.SENSOR_STATUS_ACCURACY_HIGH ->
                    Log.d(TAG, "Magnetometer accuracy: HIGH")
            }
        }
    }

    /**
     * Start reading magnetic field data.
     *
     * Gracefully returns without error if the sensor is not available.
     */
    fun start() {
        if (isRunning) return

        if (sensorManager == null || magnetometer == null) {
            Log.d(TAG, "Magnetometer not available on this device (optional sensor)")
            return
        }

        sensorManager.registerListener(
            sensorListener,
            magnetometer,
            SensorManager.SENSOR_DELAY_NORMAL
        )

        isRunning = true
        Log.i(TAG, "Magnetometer tracking started")
    }

    /** Stop reading and release sensor resources. */
    fun stop() {
        if (!isRunning) return

        sensorManager?.unregisterListener(sensorListener)
        isRunning = false
        hasReading = false
        Log.i(TAG, "Magnetometer tracking stopped")
    }

    /** Check whether this manager is actively collecting data. */
    fun isActive(): Boolean = isRunning

    /** Check whether the magnetometer is present on this device. */
    fun isAvailable(): Boolean = magnetometer != null
}
