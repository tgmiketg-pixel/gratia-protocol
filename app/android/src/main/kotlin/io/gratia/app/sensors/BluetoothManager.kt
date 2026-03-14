package io.gratia.app.sensors

import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.le.BluetoothLeScanner
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.core.content.ContextCompat
import java.security.MessageDigest

/**
 * Bluetooth manager for Proof of Life peer environment detection.
 *
 * Scans for nearby Bluetooth LE devices every 30 minutes. Each scan produces
 * a set of opaque peer hashes (SHA-256 truncated to 8 bytes) representing the
 * nearby device addresses. The Rust PoL engine compares consecutive scan sets
 * to detect "varying BT environments" — evidence that the phone has physically
 * moved between different locations (different nearby devices).
 *
 * PRIVACY: Raw Bluetooth MAC addresses are never stored or transmitted.
 * They are immediately hashed on-device and only the hash is forwarded.
 *
 * WHY periodic scanning vs. continuous: Continuous BLE scanning drains
 * significant battery. A 10-second scan every 30 minutes is sufficient to
 * capture environment snapshots for PoL while consuming negligible power.
 */
class BluetoothManager(
    private val context: Context,
    private val listener: SensorEventListener
) {

    companion object {
        private const val TAG = "GratiaBluetooth"

        // WHY: 30-minute interval provides enough snapshots to detect environment
        // changes throughout the day (up to ~48 snapshots). The PoL requirement
        // is just 2 distinct environments, so even a few successful scans suffice.
        private const val SCAN_INTERVAL_MS = 30L * 60 * 1000 // 30 minutes

        // WHY: 10-second scan window captures most nearby BLE advertisers.
        // Longer scans increase battery usage without meaningfully improving
        // environment detection for PoL purposes.
        private const val SCAN_DURATION_MS = 10L * 1000 // 10 seconds
    }

    private val handler = Handler(Looper.getMainLooper())
    private val bluetoothAdapter: BluetoothAdapter? = BluetoothAdapter.getDefaultAdapter()
    private var bleScanner: BluetoothLeScanner? = null

    private var isRunning = false

    // Peers discovered during the current scan window.
    private val currentScanPeers = mutableSetOf<Long>()

    // SHA-256 digest for hashing MAC addresses.
    private val digest: MessageDigest = MessageDigest.getInstance("SHA-256")

    private val scanCallback = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) {
            val address = result.device?.address ?: return
            val hash = hashAddress(address)
            currentScanPeers.add(hash)
        }

        override fun onBatchScanResults(results: List<ScanResult>) {
            for (result in results) {
                val address = result.device?.address ?: continue
                val hash = hashAddress(address)
                currentScanPeers.add(hash)
            }
        }

        override fun onScanFailed(errorCode: Int) {
            Log.w(TAG, "BLE scan failed with error code: $errorCode")
        }
    }

    private val scanRunnable = object : Runnable {
        override fun run() {
            if (!isRunning) return
            performScan()
            handler.postDelayed(this, SCAN_INTERVAL_MS)
        }
    }

    /**
     * Start periodic Bluetooth LE scanning.
     *
     * Requires BLUETOOTH_SCAN permission on Android 12+ or ACCESS_FINE_LOCATION
     * on older versions. Gracefully handles missing permissions or missing
     * Bluetooth hardware.
     */
    fun start() {
        if (isRunning) return

        if (bluetoothAdapter == null) {
            Log.w(TAG, "Bluetooth not available on this device")
            return
        }

        if (!hasBluetoothPermission()) {
            Log.w(TAG, "Bluetooth permissions not granted — BT PoL parameter will not be met")
            return
        }

        if (!bluetoothAdapter.isEnabled) {
            Log.w(TAG, "Bluetooth is disabled — BT PoL parameter will not be met")
            return
        }

        bleScanner = bluetoothAdapter.bluetoothLeScanner
        if (bleScanner == null) {
            Log.w(TAG, "BLE scanner not available")
            return
        }

        isRunning = true
        // Perform first scan immediately, then schedule periodic scans.
        handler.post(scanRunnable)
        Log.i(TAG, "Bluetooth LE scanning started (interval=${SCAN_INTERVAL_MS / 60000}min)")
    }

    /** Stop scanning and release resources. */
    fun stop() {
        if (!isRunning) return

        isRunning = false
        handler.removeCallbacks(scanRunnable)
        stopCurrentScan()
        Log.i(TAG, "Bluetooth LE scanning stopped")
    }

    /** Check whether this manager is actively scanning. */
    fun isActive(): Boolean = isRunning

    /** Check whether Bluetooth hardware is present. */
    fun isAvailable(): Boolean = bluetoothAdapter != null

    // ========================================================================
    // Internal
    // ========================================================================

    @SuppressLint("MissingPermission") // Permission is checked in start()
    private fun performScan() {
        val scanner = bleScanner ?: return

        if (!hasBluetoothPermission()) {
            Log.w(TAG, "Bluetooth permission lost during operation")
            return
        }

        currentScanPeers.clear()

        try {
            // WHY: LOW_POWER scan mode minimizes battery impact. We don't need
            // rapid discovery — just a snapshot of nearby advertisers.
            val settings = ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_POWER)
                .build()

            scanner.startScan(null, settings, scanCallback)
            Log.d(TAG, "BLE scan started")

            // Stop after the scan window and report results.
            handler.postDelayed({
                stopCurrentScan()
                reportScanResults()
            }, SCAN_DURATION_MS)
        } catch (e: SecurityException) {
            Log.w(TAG, "SecurityException starting BLE scan: ${e.message}")
        }
    }

    @SuppressLint("MissingPermission")
    private fun stopCurrentScan() {
        try {
            bleScanner?.stopScan(scanCallback)
        } catch (e: SecurityException) {
            Log.w(TAG, "SecurityException stopping BLE scan: ${e.message}")
        } catch (e: IllegalStateException) {
            // Scanner may already be stopped or BT turned off.
            Log.d(TAG, "BLE scanner already stopped")
        }
    }

    private fun reportScanResults() {
        val peerHashes = currentScanPeers.toList()
        if (peerHashes.isEmpty()) {
            Log.d(TAG, "BLE scan completed — no peers found")
            return
        }

        Log.d(TAG, "BLE scan completed — ${peerHashes.size} peers discovered")
        listener.onBluetoothScan(peerHashes)
    }

    /**
     * Hash a Bluetooth MAC address to an opaque 8-byte long.
     *
     * PRIVACY: Raw MAC addresses are never stored or sent across the FFI
     * boundary. The hash is a one-way transformation that preserves the
     * ability to detect "same device seen again" without revealing the
     * actual address.
     */
    private fun hashAddress(address: String): Long {
        val hashBytes = digest.digest(address.toByteArray(Charsets.UTF_8))
        digest.reset()

        // Take the first 8 bytes of the SHA-256 hash as a Long.
        var result = 0L
        for (i in 0 until 8) {
            result = result or ((hashBytes[i].toLong() and 0xFF) shl (i * 8))
        }
        return result
    }

    private fun hasBluetoothPermission(): Boolean {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            // Android 12+ requires BLUETOOTH_SCAN.
            ContextCompat.checkSelfPermission(
                context, Manifest.permission.BLUETOOTH_SCAN
            ) == PackageManager.PERMISSION_GRANTED
        } else {
            // Pre-Android 12 uses ACCESS_FINE_LOCATION for BLE scanning.
            ContextCompat.checkSelfPermission(
                context, Manifest.permission.ACCESS_FINE_LOCATION
            ) == PackageManager.PERMISSION_GRANTED
        }
    }
}
