package io.gratia.app.sensors

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.wifi.WifiManager as AndroidWifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.core.content.ContextCompat
import java.security.MessageDigest

/**
 * Wi-Fi manager for Proof of Life network detection.
 *
 * Detects visible Wi-Fi networks by triggering periodic scans and reading
 * scan results. Each BSSID (access point MAC address) is hashed to an opaque
 * long value before being sent across the FFI boundary. The Rust PoL engine
 * uses the count of distinct BSSID hashes to satisfy the "connected to at
 * least one Wi-Fi network" parameter.
 *
 * PRIVACY: Raw BSSIDs are never stored or transmitted. Only SHA-256
 * truncated hashes are forwarded.
 *
 * WHY Wi-Fi scanning interval is 20 minutes: Android throttles Wi-Fi scans
 * to 4 per 2-minute period in the foreground and 1 per 30 minutes in the
 * background (Android 9+). Our 20-minute interval stays well within the
 * background throttle limit.
 */
class GratiaWifiManager(
    private val context: Context,
    private val listener: SensorEventListener
) {

    companion object {
        private const val TAG = "GratiaWifi"

        // WHY: 20-minute interval stays within Android's background scan throttle
        // (1 scan per 30 minutes on Android 9+) with margin. Even a single
        // successful scan satisfies the Wi-Fi PoL parameter.
        private const val SCAN_INTERVAL_MS = 20L * 60 * 1000 // 20 minutes
    }

    private val handler = Handler(Looper.getMainLooper())
    private val androidWifiManager: AndroidWifiManager? =
        context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? AndroidWifiManager

    private val digest: MessageDigest = MessageDigest.getInstance("SHA-256")
    private var isRunning = false
    private var receiverRegistered = false

    // BroadcastReceiver for scan results.
    private val scanResultsReceiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context, intent: Intent) {
            if (intent.action == AndroidWifiManager.SCAN_RESULTS_AVAILABLE_ACTION) {
                val success = intent.getBooleanExtra(
                    AndroidWifiManager.EXTRA_RESULTS_UPDATED, false
                )
                processScanResults(success)
            }
        }
    }

    private val scanRunnable = object : Runnable {
        override fun run() {
            if (!isRunning) return
            triggerScan()
            handler.postDelayed(this, SCAN_INTERVAL_MS)
        }
    }

    /**
     * Start periodic Wi-Fi scanning.
     *
     * Requires ACCESS_FINE_LOCATION (or ACCESS_COARSE_LOCATION on older APIs)
     * to read scan results. Gracefully handles missing permissions.
     */
    fun start() {
        if (isRunning) return

        if (androidWifiManager == null) {
            Log.w(TAG, "WifiManager not available")
            return
        }

        if (!hasWifiPermission()) {
            Log.w(TAG, "Wi-Fi scan permission not granted — Wi-Fi PoL parameter may not be met")
            return
        }

        // Register receiver for scan results.
        val filter = IntentFilter(AndroidWifiManager.SCAN_RESULTS_AVAILABLE_ACTION)
        context.registerReceiver(scanResultsReceiver, filter)
        receiverRegistered = true

        isRunning = true

        // Read current scan results immediately (may have cached results).
        processScanResults(cached = true)

        // Schedule periodic scans.
        handler.postDelayed(scanRunnable, SCAN_INTERVAL_MS)

        Log.i(TAG, "Wi-Fi scanning started (interval=${SCAN_INTERVAL_MS / 60000}min)")
    }

    /** Stop scanning and unregister receivers. */
    fun stop() {
        if (!isRunning) return

        isRunning = false
        handler.removeCallbacks(scanRunnable)

        if (receiverRegistered) {
            try {
                context.unregisterReceiver(scanResultsReceiver)
            } catch (e: IllegalArgumentException) {
                // Receiver was already unregistered.
            }
            receiverRegistered = false
        }

        Log.i(TAG, "Wi-Fi scanning stopped")
    }

    /** Check whether this manager is actively scanning. */
    fun isActive(): Boolean = isRunning

    /** Check whether Wi-Fi hardware is present. */
    fun isAvailable(): Boolean = androidWifiManager != null

    // ========================================================================
    // Internal
    // ========================================================================

    @Suppress("DEPRECATION")
    private fun triggerScan() {
        if (!hasWifiPermission()) return

        // WHY: startScan() is deprecated in Android 9+ and throttled, but it's
        // still the only way to request a fresh scan. We also process cached
        // results from getScanResults() which works reliably.
        try {
            androidWifiManager?.startScan()
            Log.d(TAG, "Wi-Fi scan triggered")
        } catch (e: SecurityException) {
            Log.w(TAG, "SecurityException triggering Wi-Fi scan: ${e.message}")
        }
    }

    private fun processScanResults(success: Boolean = true, cached: Boolean = false) {
        if (!hasWifiPermission()) return

        try {
            @Suppress("MissingPermission")
            val results = androidWifiManager?.scanResults ?: return

            if (results.isEmpty()) {
                Log.d(TAG, "Wi-Fi scan completed — no networks found")
                return
            }

            val bssidHashes = results.mapNotNull { result ->
                val bssid = result.BSSID ?: return@mapNotNull null
                hashBssid(bssid)
            }.distinct()

            if (bssidHashes.isNotEmpty()) {
                val source = if (cached) "cached" else if (success) "fresh" else "stale"
                Log.d(TAG, "Wi-Fi scan ($source) — ${bssidHashes.size} networks found")
                listener.onWifiScan(bssidHashes)
            }
        } catch (e: SecurityException) {
            Log.w(TAG, "SecurityException reading Wi-Fi scan results: ${e.message}")
        }
    }

    /**
     * Hash a BSSID (MAC address) to an opaque 8-byte long.
     *
     * Same hashing approach as BluetoothManager for consistency.
     */
    private fun hashBssid(bssid: String): Long {
        val hashBytes = digest.digest(bssid.toByteArray(Charsets.UTF_8))
        digest.reset()

        var result = 0L
        for (i in 0 until 8) {
            result = result or ((hashBytes[i].toLong() and 0xFF) shl (i * 8))
        }
        return result
    }

    private fun hasWifiPermission(): Boolean {
        // WHY: ACCESS_FINE_LOCATION is required to get Wi-Fi scan results on
        // Android 8.0+. ACCESS_COARSE_LOCATION is insufficient for BSSID data.
        return ContextCompat.checkSelfPermission(
            context, Manifest.permission.ACCESS_FINE_LOCATION
        ) == PackageManager.PERMISSION_GRANTED
    }
}
