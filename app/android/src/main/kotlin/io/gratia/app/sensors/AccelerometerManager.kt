package io.gratia.app.sensors

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener as AndroidSensorEventListener
import android.hardware.SensorManager
import android.util.Log
import kotlin.math.sqrt

/**
 * Accelerometer manager for Proof of Life motion detection.
 *
 * Listens to TYPE_ACCELEROMETER at SENSOR_DELAY_NORMAL (lowest power) and
 * runs a lightweight classifier to distinguish human-consistent motion from
 * a stationary device. Emits [SensorEventListener.onMotion] when motion
 * exceeds the human-movement threshold, and [SensorEventListener.onOrientationChange]
 * when a significant orientation shift is detected.
 *
 * PRIVACY: Raw accelerometer readings are never stored or transmitted.
 * Only the boolean "human motion detected" / "orientation changed" flags
 * cross the FFI boundary.
 */
class AccelerometerManager(
    private val context: Context,
    private val listener: SensorEventListener
) {

    companion object {
        private const val TAG = "GratiaAccelerometer"

        // WHY: Gravity is ~9.81 m/s^2. We subtract it to get linear acceleration.
        // A magnitude above this threshold indicates the phone is being carried
        // or handled by a human rather than sitting on a desk.
        private const val MOTION_THRESHOLD = 1.5f // m/s^2 above gravity

        // WHY: Orientation is considered changed when the dominant gravity axis
        // shifts by more than 30 degrees equivalent. This detects picking up
        // the phone, rotating it, or setting it down.
        private const val ORIENTATION_THRESHOLD = 3.0f // m/s^2 shift in any axis

        // WHY: Debounce prevents flooding the PoL engine with redundant events.
        // One motion detection per 5 minutes is sufficient for the daily PoL flag.
        private const val MOTION_DEBOUNCE_MS = 5L * 60 * 1000 // 5 minutes

        // WHY: Orientation events are rarer and more meaningful — 2-minute debounce.
        private const val ORIENTATION_DEBOUNCE_MS = 2L * 60 * 1000 // 2 minutes

        // WHY: We need a few samples to compute a stable baseline gravity vector.
        // 10 samples at SENSOR_DELAY_NORMAL (~200ms each) takes ~2 seconds.
        private const val BASELINE_SAMPLE_COUNT = 10
    }

    private val sensorManager = context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager
    private val accelerometer = sensorManager?.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)

    private var isRunning = false
    private var lastMotionEventMs = 0L
    private var lastOrientationEventMs = 0L

    // Gravity baseline — estimated via low-pass filter.
    private var gravityX = 0f
    private var gravityY = 0f
    private var gravityZ = 0f
    private var baselineSamplesCollected = 0
    private var baselineEstablished = false

    // Previous gravity vector for orientation change detection.
    private var prevGravityX = 0f
    private var prevGravityY = 0f
    private var prevGravityZ = 0f

    private val sensorListener = object : AndroidSensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            if (event.sensor.type != Sensor.TYPE_ACCELEROMETER) return
            processAccelerometerReading(event.values[0], event.values[1], event.values[2])
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {
            // No-op — accuracy changes don't affect our classification.
        }
    }

    /**
     * Start listening to accelerometer data.
     *
     * Gracefully handles the sensor being unavailable (some very low-end
     * devices may lack an accelerometer, though this is rare post-2018).
     */
    fun start() {
        if (isRunning) return

        if (sensorManager == null || accelerometer == null) {
            Log.w(TAG, "Accelerometer not available on this device")
            return
        }

        // WHY: SENSOR_DELAY_NORMAL is the lowest power sampling rate (~5 Hz).
        // We don't need high-frequency data — just enough to detect motion
        // patterns over the course of the day.
        sensorManager.registerListener(
            sensorListener,
            accelerometer,
            SensorManager.SENSOR_DELAY_NORMAL
        )

        isRunning = true
        Log.i(TAG, "Accelerometer tracking started")
    }

    /** Stop listening and release sensor resources. */
    fun stop() {
        if (!isRunning) return

        sensorManager?.unregisterListener(sensorListener)
        isRunning = false
        baselineEstablished = false
        baselineSamplesCollected = 0
        Log.i(TAG, "Accelerometer tracking stopped")
    }

    /** Check whether this manager is actively collecting data. */
    fun isActive(): Boolean = isRunning

    /** Check whether the accelerometer sensor is present on this device. */
    fun isAvailable(): Boolean = accelerometer != null

    // ========================================================================
    // Internal — Motion Classification
    // ========================================================================

    private fun processAccelerometerReading(x: Float, y: Float, z: Float) {
        // Low-pass filter to estimate gravity component.
        // WHY: The accelerometer reading includes both gravity and linear
        // acceleration. We separate them with an exponential moving average.
        // Alpha of 0.8 heavily favors previous values, giving a stable gravity
        // estimate that changes slowly.
        val alpha = 0.8f
        gravityX = alpha * gravityX + (1 - alpha) * x
        gravityY = alpha * gravityY + (1 - alpha) * y
        gravityZ = alpha * gravityZ + (1 - alpha) * z

        if (!baselineEstablished) {
            baselineSamplesCollected++
            if (baselineSamplesCollected >= BASELINE_SAMPLE_COUNT) {
                baselineEstablished = true
                prevGravityX = gravityX
                prevGravityY = gravityY
                prevGravityZ = gravityZ
                Log.d(TAG, "Gravity baseline established: ($gravityX, $gravityY, $gravityZ)")
            }
            return
        }

        // Linear acceleration = total acceleration - gravity.
        val linearX = x - gravityX
        val linearY = y - gravityY
        val linearZ = z - gravityZ
        val linearMagnitude = sqrt(linearX * linearX + linearY * linearY + linearZ * linearZ)

        val now = System.currentTimeMillis()

        // Motion detection — human carrying or handling the phone.
        if (linearMagnitude > MOTION_THRESHOLD) {
            if (now - lastMotionEventMs > MOTION_DEBOUNCE_MS) {
                lastMotionEventMs = now
                Log.d(TAG, "Human motion detected (magnitude=$linearMagnitude)")
                listener.onMotion()
            }
        }

        // Orientation change detection — phone picked up, rotated, or set down.
        val gravityShiftX = gravityX - prevGravityX
        val gravityShiftY = gravityY - prevGravityY
        val gravityShiftZ = gravityZ - prevGravityZ
        val gravityShiftMag = sqrt(
            gravityShiftX * gravityShiftX +
                gravityShiftY * gravityShiftY +
                gravityShiftZ * gravityShiftZ
        )

        if (gravityShiftMag > ORIENTATION_THRESHOLD) {
            if (now - lastOrientationEventMs > ORIENTATION_DEBOUNCE_MS) {
                lastOrientationEventMs = now
                prevGravityX = gravityX
                prevGravityY = gravityY
                prevGravityZ = gravityZ
                Log.d(TAG, "Orientation change detected (shift=$gravityShiftMag)")
                listener.onOrientationChange()
            }
        }
    }
}
