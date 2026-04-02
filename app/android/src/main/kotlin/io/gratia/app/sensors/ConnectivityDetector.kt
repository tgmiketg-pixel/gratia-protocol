package io.gratia.app.sensors

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.telephony.TelephonyManager
import android.util.Log

/**
 * Detects SIM card presence and active network type to determine the optimal
 * transport strategy for connecting to the Gratia blockchain network.
 *
 * WHY: Devices without a SIM card (like the Samsung A06 test unit) have broken
 * UDP/QUIC sockets on some Android builds — the carrier firmware cripples UDP
 * when no cellular radio is active. By detecting this upfront, we skip QUIC
 * entirely and go straight to TCP, avoiding the timeout-then-fallback delay.
 */
class ConnectivityDetector(private val context: Context) {

    companion object {
        private const val TAG = "ConnectivityDetector"
    }

    enum class ConnectionProfile {
        /** SIM present, cellular or Wi-Fi available — use QUIC primary, TCP fallback */
        FULL,
        /** No SIM, Wi-Fi available — use TCP primary, skip QUIC */
        WIFI_ONLY,
        /** No SIM, no Wi-Fi — Bluetooth mesh relay only */
        OFFLINE
    }

    /** Current detected profile. Updated on connectivity changes. */
    var currentProfile: ConnectionProfile = detect()
        private set

    private var listener: ((ConnectionProfile) -> Unit)? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    /**
     * Detect current connection profile based on SIM state and active network.
     */
    fun detect(): ConnectionProfile {
        val hasSim = hasSimCard()
        val hasWifi = hasWifiConnectivity()
        val hasCellular = hasCellularConnectivity()

        val profile = when {
            hasSim && (hasCellular || hasWifi) -> ConnectionProfile.FULL
            hasSim && !hasCellular && !hasWifi -> ConnectionProfile.OFFLINE
            !hasSim && hasWifi -> ConnectionProfile.WIFI_ONLY
            !hasSim && !hasWifi -> ConnectionProfile.OFFLINE
            else -> ConnectionProfile.WIFI_ONLY
        }

        Log.i(TAG, "Connection profile: $profile (sim=$hasSim, wifi=$hasWifi, cellular=$hasCellular)")
        currentProfile = profile
        return profile
    }

    /**
     * Check if any SIM card is present and ready.
     * Works on all Android versions without special permissions.
     */
    private fun hasSimCard(): Boolean {
        val tm = context.getSystemService(Context.TELEPHONY_SERVICE) as? TelephonyManager
            ?: return false

        return when (tm.simState) {
            TelephonyManager.SIM_STATE_READY -> true
            TelephonyManager.SIM_STATE_PIN_REQUIRED,
            TelephonyManager.SIM_STATE_PUK_REQUIRED,
            TelephonyManager.SIM_STATE_NETWORK_LOCKED -> true  // SIM present but locked
            TelephonyManager.SIM_STATE_ABSENT -> false
            TelephonyManager.SIM_STATE_UNKNOWN -> false
            else -> false
        }
    }

    /**
     * Check if Wi-Fi transport is currently active.
     */
    private fun hasWifiConnectivity(): Boolean {
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return false
        val network = cm.activeNetwork ?: return false
        val caps = cm.getNetworkCapabilities(network) ?: return false
        return caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)
    }

    /**
     * Check if cellular transport is currently active.
     */
    private fun hasCellularConnectivity(): Boolean {
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return false
        val network = cm.activeNetwork ?: return false
        val caps = cm.getNetworkCapabilities(network) ?: return false
        return caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)
    }

    /**
     * Register for connectivity changes. When the profile changes (e.g., Wi-Fi
     * drops, SIM inserted), the listener is called so the network layer can
     * reconfigure transports on the fly.
     */
    fun startMonitoring(onProfileChanged: (ConnectionProfile) -> Unit) {
        listener = onProfileChanged

        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return

        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()

        networkCallback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                val newProfile = detect()
                if (newProfile != currentProfile) {
                    currentProfile = newProfile
                    Log.i(TAG, "Profile changed → $newProfile (network available)")
                    listener?.invoke(newProfile)
                }
            }

            override fun onLost(network: Network) {
                val newProfile = detect()
                if (newProfile != currentProfile) {
                    currentProfile = newProfile
                    Log.i(TAG, "Profile changed → $newProfile (network lost)")
                    listener?.invoke(newProfile)
                }
            }

            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                val newProfile = detect()
                if (newProfile != currentProfile) {
                    currentProfile = newProfile
                    Log.i(TAG, "Profile changed → $newProfile (capabilities changed)")
                    listener?.invoke(newProfile)
                }
            }
        }

        cm.registerNetworkCallback(request, networkCallback!!)
        Log.i(TAG, "Monitoring connectivity changes")
    }

    /**
     * Stop monitoring connectivity changes.
     */
    fun stopMonitoring() {
        networkCallback?.let { cb ->
            val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            cm?.unregisterNetworkCallback(cb)
        }
        networkCallback = null
        listener = null
        Log.i(TAG, "Stopped monitoring connectivity changes")
    }
}
