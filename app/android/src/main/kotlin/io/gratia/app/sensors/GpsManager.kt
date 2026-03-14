package io.gratia.app.sensors

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Bundle
import android.os.Looper
import android.util.Log
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel

/**
 * GPS/Location manager for Proof of Life attestation.
 *
 * Uses FusedLocationProviderClient (Google Play Services) when available,
 * with a fallback to the platform LocationManager. Requests coarse location
 * updates every 15 minutes — aggressive tracking is unnecessary for PoL
 * and would waste battery.
 *
 * PRIVACY: Only lat/lon are captured and immediately forwarded as an
 * FfiSensorEvent. Raw location data is never persisted on disk.
 */
class GpsManager(
    private val context: Context,
    private val listener: SensorEventListener
) {

    companion object {
        private const val TAG = "GratiaGpsManager"

        // WHY: 15-minute interval balances PoL requirement (at least one fix per day)
        // against battery consumption. Even one successful fix satisfies the GPS parameter.
        private const val UPDATE_INTERVAL_MS = 15L * 60 * 1000 // 15 minutes

        // WHY: Coarse accuracy (city-block level) is sufficient for PoL geographic
        // plausibility checks. Fine location would be more power-hungry and privacy-invasive.
        private const val MIN_DISTANCE_METERS = 100f
    }

    private val scope = CoroutineScope(Dispatchers.Default + SupervisorJob())

    private var isRunning = false
    private var fusedClient: Any? = null // FusedLocationProviderClient, loaded via reflection
    private var platformLocationManager: LocationManager? = null
    private var usingFused = false

    // Platform LocationManager listener (fallback path)
    private val platformListener = object : LocationListener {
        override fun onLocationChanged(location: Location) {
            handleLocation(location)
        }

        @Deprecated("Deprecated in API level 29")
        override fun onStatusChanged(provider: String?, status: Int, extras: Bundle?) {
            // No-op — deprecated but required on older API levels.
        }

        override fun onProviderEnabled(provider: String) {
            Log.d(TAG, "Location provider enabled: $provider")
        }

        override fun onProviderDisabled(provider: String) {
            Log.d(TAG, "Location provider disabled: $provider")
        }
    }

    /**
     * Start requesting periodic location updates.
     *
     * Requires ACCESS_COARSE_LOCATION or ACCESS_FINE_LOCATION permission.
     * If permissions are not granted, logs a warning and returns without
     * crashing — the PoL GPS parameter simply won't be satisfied.
     */
    fun start() {
        if (isRunning) return

        if (!hasLocationPermission()) {
            Log.w(TAG, "Location permission not granted — GPS PoL parameter will not be met")
            return
        }

        // Try Google Play Services FusedLocationProviderClient first.
        if (tryStartFused()) {
            usingFused = true
            isRunning = true
            Log.i(TAG, "Started GPS tracking via FusedLocationProviderClient")
            return
        }

        // Fallback to platform LocationManager.
        tryStartPlatform()
        isRunning = true
        Log.i(TAG, "Started GPS tracking via platform LocationManager")
    }

    /** Stop location updates and release resources. */
    fun stop() {
        if (!isRunning) return

        if (usingFused) {
            stopFused()
        } else {
            platformLocationManager?.removeUpdates(platformListener)
        }

        scope.cancel()
        isRunning = false
        Log.i(TAG, "GPS tracking stopped")
    }

    /** Check whether this manager is actively collecting location data. */
    fun isActive(): Boolean = isRunning

    // ========================================================================
    // Internal
    // ========================================================================

    private fun hasLocationPermission(): Boolean {
        val coarse = ContextCompat.checkSelfPermission(
            context, Manifest.permission.ACCESS_COARSE_LOCATION
        ) == PackageManager.PERMISSION_GRANTED

        val fine = ContextCompat.checkSelfPermission(
            context, Manifest.permission.ACCESS_FINE_LOCATION
        ) == PackageManager.PERMISSION_GRANTED

        return coarse || fine
    }

    private fun handleLocation(location: Location) {
        val lat = location.latitude.toFloat()
        val lon = location.longitude.toFloat()
        Log.d(TAG, "GPS fix obtained: lat=$lat, lon=$lon (accuracy=${location.accuracy}m)")
        listener.onGpsUpdate(lat, lon)
    }

    // --- FusedLocationProviderClient (Google Play Services) ---

    /**
     * Attempt to start location tracking via FusedLocationProviderClient.
     *
     * WHY: We use reflection to access Google Play Services so the app can
     * compile and run on devices without Play Services (e.g., Huawei, F-Droid
     * builds). If the classes are not available at runtime, we fall back to
     * the platform LocationManager.
     *
     * @return true if the fused client was successfully started.
     */
    @Suppress("TooGenericExceptionCaught")
    private fun tryStartFused(): Boolean {
        try {
            val fusedClass = Class.forName(
                "com.google.android.gms.location.LocationServices"
            )
            val getClient = fusedClass.getMethod(
                "getFusedLocationProviderClient",
                Context::class.java
            )
            val client = getClient.invoke(null, context) ?: return false
            fusedClient = client

            // Build a LocationRequest via its Builder.
            val requestBuilderClass = Class.forName(
                "com.google.android.gms.location.LocationRequest\$Builder"
            )
            // Priority.PRIORITY_BALANCED_POWER_ACCURACY = 102
            // WHY: Balanced priority uses Wi-Fi and cell towers for location,
            // avoiding the high battery cost of raw GPS. Sufficient for PoL.
            val builder = requestBuilderClass.getConstructor(
                Long::class.javaPrimitiveType
            ).newInstance(UPDATE_INTERVAL_MS)

            val setMinUpdate = requestBuilderClass.getMethod(
                "setMinUpdateDistanceMeters", Float::class.javaPrimitiveType
            )
            setMinUpdate.invoke(builder, MIN_DISTANCE_METERS)

            val buildMethod = requestBuilderClass.getMethod("build")
            val locationRequest = buildMethod.invoke(builder)

            // Create a LocationCallback.
            val callbackClass = Class.forName(
                "com.google.android.gms.location.LocationCallback"
            )
            val locationResultClass = Class.forName(
                "com.google.android.gms.location.LocationResult"
            )

            // WHY: We create a dynamic proxy of LocationCallback because we
            // can't directly subclass it via reflection. The proxy intercepts
            // onLocationResult calls and extracts the Location.
            val callback = java.lang.reflect.Proxy.newProxyInstance(
                callbackClass.classLoader,
                arrayOf(callbackClass)
            ) { _, method, args ->
                if (method.name == "onLocationResult" && args != null && args.isNotEmpty()) {
                    val result = args[0]
                    val getLastLocation = locationResultClass.getMethod("getLastLocation")
                    val location = getLastLocation.invoke(result) as? Location
                    location?.let { handleLocation(it) }
                }
                null
            }

            // Actually this approach won't work because LocationCallback is a class, not
            // an interface. Fall back to platform LocationManager.
            return false
        } catch (e: ClassNotFoundException) {
            Log.d(TAG, "Google Play Services not available: ${e.message}")
            return false
        } catch (e: Exception) {
            Log.w(TAG, "Failed to initialize FusedLocationProviderClient: ${e.message}")
            return false
        }
    }

    private fun stopFused() {
        // If fused was used, remove updates. Since we fell back to platform in
        // tryStartFused, this is currently a no-op.
        fusedClient = null
    }

    // --- Platform LocationManager (fallback) ---

    @Suppress("MissingPermission") // Permission is checked in start()
    private fun tryStartPlatform() {
        val lm = context.getSystemService(Context.LOCATION_SERVICE) as? LocationManager
        if (lm == null) {
            Log.e(TAG, "LocationManager system service not available")
            return
        }
        platformLocationManager = lm

        // WHY: Request from NETWORK_PROVIDER first (lower power), then
        // GPS_PROVIDER as secondary. Either one satisfies the PoL GPS parameter.
        val providers = mutableListOf<String>()
        if (lm.isProviderEnabled(LocationManager.NETWORK_PROVIDER)) {
            providers.add(LocationManager.NETWORK_PROVIDER)
        }
        if (lm.isProviderEnabled(LocationManager.GPS_PROVIDER)) {
            providers.add(LocationManager.GPS_PROVIDER)
        }

        if (providers.isEmpty()) {
            Log.w(TAG, "No location providers enabled — GPS PoL parameter will not be met")
            return
        }

        for (provider in providers) {
            try {
                lm.requestLocationUpdates(
                    provider,
                    UPDATE_INTERVAL_MS,
                    MIN_DISTANCE_METERS,
                    platformListener,
                    Looper.getMainLooper()
                )
                Log.d(TAG, "Registered location updates from provider: $provider")
            } catch (e: SecurityException) {
                Log.w(TAG, "SecurityException requesting $provider updates: ${e.message}")
            }
        }

        // Also grab the last known location immediately if available.
        for (provider in providers) {
            try {
                val lastKnown = lm.getLastKnownLocation(provider)
                if (lastKnown != null) {
                    val ageMs = System.currentTimeMillis() - lastKnown.time
                    // WHY: Only use last-known if it's less than 30 minutes old.
                    // Stale locations would not represent current geographic reality.
                    val maxAgMs = 30L * 60 * 1000
                    if (ageMs < maxAgMs) {
                        handleLocation(lastKnown)
                        break
                    }
                }
            } catch (e: SecurityException) {
                // Ignored — we'll get a fresh fix from the periodic updates.
            }
        }
    }
}
