package io.gratia.app

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build
import android.util.Log
import io.gratia.app.bridge.GratiaCoreManager

/**
 * Application class for Gratia.
 *
 * Initializes core infrastructure at app startup:
 * - Logging
 * - Notification channels for foreground services
 * - Rust core bridge (GratiaNode) via GratiaCoreManager
 */
class GratiaApplication : Application() {

    companion object {
        private const val TAG = "GratiaApplication"

        // Notification channel IDs for foreground services
        const val CHANNEL_POL = "gratia_proof_of_life"
        const val CHANNEL_MINING = "gratia_mining"
    }

    override fun onCreate() {
        super.onCreate()

        Log.i(TAG, "Gratia application starting")

        createNotificationChannels()
        initializeRustCore()
    }

    /**
     * Create notification channels for foreground services.
     *
     * WHY: Android 8.0+ (our minSdk) requires notification channels for all
     * notifications. The PoL and Mining services run as foreground services
     * which must display a persistent notification.
     */
    private fun createNotificationChannels() {
        val notificationManager = getSystemService(NotificationManager::class.java)

        val polChannel = NotificationChannel(
            CHANNEL_POL,
            getString(R.string.channel_pol_name),
            // WHY: LOW importance so the PoL notification doesn't make sound or
            // vibrate. The user should never be interrupted by passive PoL collection.
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = getString(R.string.channel_pol_description)
        }

        val miningChannel = NotificationChannel(
            CHANNEL_MINING,
            getString(R.string.channel_mining_name),
            // WHY: DEFAULT importance for mining — user should be aware mining is
            // active since it uses CPU resources (only while plugged in + above 80%).
            NotificationManager.IMPORTANCE_DEFAULT
        ).apply {
            description = getString(R.string.channel_mining_description)
        }

        notificationManager.createNotificationChannel(polChannel)
        notificationManager.createNotificationChannel(miningChannel)

        Log.d(TAG, "Notification channels created")
    }

    /**
     * Initialize the Rust core via UniFFI bridge.
     *
     * The GratiaNode is the single entry point for all protocol operations.
     * It is created once at app launch and held for the lifetime of the app.
     */
    private fun initializeRustCore() {
        val dataDir = filesDir.absolutePath
        Log.i(TAG, "Initializing Rust core with data dir: $dataDir")

        try {
            GratiaCoreManager.initialize(dataDir)
            Log.i(TAG, "Rust core initialized successfully")

            // Auto-create a wallet on first launch if one doesn't exist.
            // WHY: The consensus engine needs a signing key (derived from the wallet)
            // for VRF block producer selection. Without a wallet, consensus can't start.
            // This matches the onboarding design: "install, use phone normally" — no
            // manual wallet creation step required.
            try {
                val address = GratiaCoreManager.createWallet()
                Log.i(TAG, "Wallet created: $address")
            } catch (e: Exception) {
                // Wallet already exists — this is fine (normal on subsequent launches)
                Log.d(TAG, "Wallet already exists or creation skipped: ${e.message}")
            }
        } catch (e: Exception) {
            // WHY: We log but don't crash here. The app can still display the UI
            // and will show appropriate error states. This handles the case where
            // the native .so library isn't loaded yet during development.
            Log.e(TAG, "Failed to initialize Rust core: ${e.message}", e)
        }
    }
}
