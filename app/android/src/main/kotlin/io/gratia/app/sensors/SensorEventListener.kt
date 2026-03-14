package io.gratia.app.sensors

/**
 * Common listener interface for all sensor managers.
 *
 * Each sensor manager emits events through this callback. The events map
 * directly to [FfiSensorEvent] variants defined in the Rust UniFFI bridge.
 * The ProofOfLifeService collects these and forwards them to GratiaNode
 * via [GratiaNode.submitSensorEvent].
 */
interface SensorEventListener {

    /** Phone was unlocked by the user. */
    fun onUnlock()

    /** A screen interaction session was recorded. */
    fun onInteraction(durationSecs: Int)

    /** Phone orientation changed (picked up, rotated, set down). */
    fun onOrientationChange()

    /** Accelerometer detected human-consistent motion. */
    fun onMotion()

    /** A GPS fix was obtained. */
    fun onGpsUpdate(lat: Float, lon: Float)

    /** Wi-Fi scan completed with visible BSSIDs (as opaque hashes). */
    fun onWifiScan(bssidHashes: List<Long>)

    /** Bluetooth scan completed with nearby peers (as opaque hashes). */
    fun onBluetoothScan(peerHashes: List<Long>)

    /** Charge state changed (plugged in or unplugged). */
    fun onChargeEvent(isCharging: Boolean)
}
