package io.gratia.app

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import io.gratia.app.BuildConfig
import io.gratia.app.bridge.GratiaCoreManager
import io.gratia.app.bridge.GratiaBridgeException
import io.gratia.app.bridge.NetworkStatus
import io.gratia.app.security.HardwareKeystore
import io.gratia.app.service.ProofOfLifeService
import io.gratia.app.worker.PolHeartbeatWorker
import java.util.concurrent.TimeUnit

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

        /** Global app context for ViewModels that need to post notifications. */
        var appContext: Context? = null
            private set
    }

    // WHY: Tracks whether wallet initialization succeeded. Network and consensus
    // must not start without a wallet because the consensus engine needs a signing
    // key derived from the wallet for VRF block producer selection.
    private var walletReady = false

    override fun onCreate() {
        super.onCreate()

        Log.i(TAG, "Gratia application starting")
        appContext = applicationContext

        io.gratia.app.security.AddressBook.init(this)
        createNotificationChannels()
        initializeRustCore()
        startP2PNetwork()
        schedulePolHeartbeat()
        // WHY: PoL service is started from MainActivity after runtime permissions
        // (location, Bluetooth) are granted. Android 14+ (targetSdk 34) requires
        // ACCESS_FINE_LOCATION before starting a foreground service with type "location".
    }

    /**
     * Start the Proof of Life background service.
     *
     * WHY: PoL data collection must begin as soon as the app launches.
     * The service runs as a foreground service with a silent notification,
     * collecting sensor events (unlocks, GPS, accelerometer, etc.) and
     * forwarding them to the Rust core. This is what makes the PoL checklist
     * on the Mining tab fill in throughout the day.
     */
    private fun startProofOfLifeService() {
        if (!GratiaCoreManager.isInitialized) {
            Log.w(TAG, "Skipping PoL service start — core not initialized")
            return
        }

        try {
            val intent = Intent(this, ProofOfLifeService::class.java)
            startForegroundService(intent)
            Log.i(TAG, "Proof of Life service started")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start PoL service: ${e.message}", e)
        }
    }

    /**
     * Start the P2P network layer (libp2p with Gossipsub + mDNS).
     *
     * WHY: The network must be running for peer discovery (mDNS), block/transaction
     * gossip, and consensus participation. Without this call, phones on the same
     * Wi-Fi never discover each other and no gossip or consensus traffic flows.
     * Started at app launch so the node is reachable as early as possible.
     *
     * Tries port 9000 first for demo connectivity, then falls back through
     * 9001-9010 if the port is already bound.
     */
    private fun startP2PNetwork() {
        if (!GratiaCoreManager.isInitialized) {
            Log.w(TAG, "Skipping P2P network start — core not initialized")
            return
        }

        if (!walletReady) {
            Log.w(TAG, "Skipping P2P network start — no wallet available")
            return
        }

        try {
            // WHY: Try port 9000 first for the demo so phones can connect to each
            // other at a known address. If 9000 is already bound (e.g., network
            // already running or another process), try ports 9001-9010 before
            // giving up. In production, use port 0 (OS-assigned).
            val BASE_PORT = 9000
            // WHY: 11 attempts (9000-9010) covers common scenarios like a stale
            // process holding the port or multiple Gratia instances during testing.
            val MAX_PORT_ATTEMPTS = 11
            var status: NetworkStatus? = null
            for (port in BASE_PORT until BASE_PORT + MAX_PORT_ATTEMPTS) {
                try {
                    status = GratiaCoreManager.startNetwork(listenPort = port)
                    Log.i(TAG, "P2P network started on port $port")
                    break
                } catch (e: Exception) {
                    if (port < BASE_PORT + MAX_PORT_ATTEMPTS - 1) {
                        Log.w(TAG, "Port $port unavailable, trying ${port + 1}: ${e.message}")
                    } else {
                        Log.e(TAG, "All ports $BASE_PORT-${port} failed, network may already be running: ${e.message}")
                    }
                }
            }
            if (status == null) {
                Log.w(TAG, "P2P network start failed on all ports — network may already be running, continuing with staged startup")
            }
            Log.i(TAG, "P2P network status — listening on: ${status?.listenAddress ?: "unknown"}, peers: ${status?.peerCount ?: 0}")

            // WHY: Start everything immediately on a background thread.
            // No artificial delays — the bootstrap node is a known address
            // so peer discovery is near-instant. The thread avoids blocking
            // the main thread (which would freeze the UI).
            Thread {
                // Explorer API + GratiaVM — no peer dependency
                try {
                    val url = GratiaCoreManager.startExplorerApi(8080)
                    Log.i(TAG, "Explorer API started: $url")
                } catch (e: Exception) {
                    Log.w(TAG, "Explorer API start failed: ${e.message}")
                }
                try {
                    val contracts = GratiaCoreManager.initVm()
                    Log.i(TAG, "GratiaVM initialized: ${contracts.size} contracts deployed")
                } catch (e: Exception) {
                    Log.w(TAG, "GratiaVM init failed: ${e.message}")
                }

                try {
                    val consensusStatus = GratiaCoreManager.startConsensus()
                    Log.i(TAG, "Consensus started — slot: ${consensusStatus.currentSlot}, committee: ${consensusStatus.isCommitteeMember}")
                } catch (e: Exception) {
                    Log.w(TAG, "Consensus start failed (may already be running): ${e.message}")
                }

                // WHY: Auto-start mining so users earn GRAT immediately without
                // needing to open the Mining tab. Also starts the MiningService
                // foreground notification so the user sees mining is active.
                try {
                    // Update power state so the Rust core knows we're plugged in
                    val batteryManager = getSystemService(Context.BATTERY_SERVICE) as? android.os.BatteryManager
                    val isCharging = batteryManager?.isCharging ?: false
                    val batteryPct = batteryManager?.getIntProperty(android.os.BatteryManager.BATTERY_PROPERTY_CAPACITY) ?: 0
                    GratiaCoreManager.updatePowerState(isCharging, batteryPct)
                    Log.i(TAG, "Power state updated: charging=$isCharging, battery=$batteryPct%")

                    // Start mining in the Rust core and MiningService (unless user previously stopped)
                    if (!GratiaCoreManager.userStoppedMining) {
                        GratiaCoreManager.startMining()
                        Log.i(TAG, "Mining auto-started")

                        // Start the Android MiningService for the persistent notification
                        val miningIntent = Intent(this@GratiaApplication, io.gratia.app.service.MiningService::class.java)
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                            startForegroundService(miningIntent)
                        } else {
                            startService(miningIntent)
                        }
                        Log.i(TAG, "MiningService started")
                    } else {
                        Log.i(TAG, "Mining skipped — user previously stopped")
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "Auto-start mining failed: ${e.message}")
                }
            }.start()
        } catch (e: GratiaBridgeException) {
            Log.e(TAG, "Failed to start P2P network: ${e.message}", e)
        } catch (e: Exception) {
            Log.e(TAG, "Unexpected error starting P2P network: ${e.message}", e)
        }
    }

    /**
     * Schedule a periodic WorkManager task that checks if ProofOfLifeService
     * is alive and restarts it if needed.
     *
     * WHY: Android OEMs (Xiaomi, Samsung, Huawei, Oppo) aggressively kill
     * background services beyond what stock Android does. START_STICKY alone
     * is insufficient on these devices. WorkManager survives app kills, doze
     * mode, and OEM battery optimizations because it delegates to JobScheduler
     * (API 23+) which the OS treats as a first-class scheduled task.
     *
     * 15 minutes is the minimum periodic interval WorkManager allows.
     * KEEP policy ensures we never have duplicate heartbeat workers running.
     */
    private fun schedulePolHeartbeat() {
        try {
            val heartbeatRequest = PeriodicWorkRequestBuilder<PolHeartbeatWorker>(
                // WHY: 15 minutes is the minimum interval WorkManager permits.
                // Shorter values are silently clamped to 15 minutes.
                15, TimeUnit.MINUTES
            ).build()

            WorkManager.getInstance(this).enqueueUniquePeriodicWork(
                PolHeartbeatWorker.WORK_NAME,
                // WHY: KEEP preserves the existing scheduled work if it already exists.
                // This prevents resetting the timer on every app launch.
                ExistingPeriodicWorkPolicy.KEEP,
                heartbeatRequest
            )

            Log.i(TAG, "PoL heartbeat worker scheduled (15-minute interval)")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to schedule PoL heartbeat worker: ${e.message}", e)
        }
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

            // WHY: Enable debug bypass in debug builds so we can test mining
            // and transactions without waiting 24 hours for PoL to complete.
            if (BuildConfig.DEBUG) {
                GratiaCoreManager.enableDebugBypass()
                Log.i(TAG, "Debug bypass enabled (debug build)")
            }

            // WHY: Initialize the hardware keystore BEFORE checking for an
            // existing wallet. The AES wrapping key must exist in Android
            // Keystore before we can migrate or store any wallet keys.
            initializeHardwareKeystore(dataDir)

            // Auto-create a wallet on first launch if one doesn't exist.
            // WHY: The consensus engine needs a signing key (derived from the wallet)
            // for VRF block producer selection. Without a wallet, consensus can't start.
            // This matches the onboarding design: "install, use phone normally" — no
            // manual wallet creation step required.
            //
            // WHY: FileKeystore now auto-loads the key from disk if it exists, so
            // getWalletInfo() succeeding means we already have a wallet. We only
            // call createWallet() when no key file was found. If both fail,
            // network and consensus cannot function — so we abort startup.
            try {
                val info = GratiaCoreManager.getWalletInfo()
                Log.i(TAG, "Wallet loaded from file: ${info.address}")
                walletReady = true
            } catch (e: Exception) {
                // No existing wallet — create a new one
                try {
                    val address = GratiaCoreManager.createWallet()
                    Log.i(TAG, "Wallet created: $address")
                    walletReady = true
                } catch (e2: Exception) {
                    Log.e(TAG, "FATAL: Failed to create wallet AND no existing wallet found. " +
                        "Network and consensus will NOT start. Error: ${e2.message}", e2)
                }
            }

            if (!walletReady) {
                Log.e(TAG, "FATAL: No wallet available — skipping network/consensus startup")
                return
            }

        } catch (e: Exception) {
            // WHY: We log but don't crash here. The app can still display the UI
            // and will show appropriate error states. This handles the case where
            // the native .so library isn't loaded yet during development.
            Log.e(TAG, "Failed to initialize Rust core: ${e.message}", e)
        }
    }

    /**
     * Initialize the Android Keystore-backed hardware keystore and migrate
     * any existing plaintext wallet key to encrypted storage.
     *
     * WHY: Before this integration, FileKeystore stored the raw Ed25519 key
     * in plaintext at {dataDir}/wallet_key.bin. While Android's app sandbox
     * restricts access to the file, a rooted device or physical extraction
     * could read the key. By encrypting it with an AES key held in the
     * hardware security module (StrongBox or TEE), the private key is
     * protected even if the file system is compromised.
     *
     * Migration is automatic and one-time: if a plaintext key file exists,
     * it is encrypted, stored in SharedPreferences, and the original file
     * is overwritten with zeros then deleted.
     */
    private fun initializeHardwareKeystore(dataDir: String) {
        try {
            HardwareKeystore.init(applicationContext)
            Log.i(TAG, "HardwareKeystore initialized (hardware-backed: ${HardwareKeystore.isHardwareBacked()})")

            // WHY: Migrate plaintext key file to hardware keystore. This is a
            // one-time operation on first launch after the hardware keystore
            // integration is deployed. On subsequent launches, the plaintext
            // file will no longer exist and this returns false immediately.
            if (HardwareKeystore.migrateFromPlaintextFile(dataDir)) {
                Log.i(TAG, "Migrated plaintext wallet key to hardware-backed encrypted storage")
            }
        } catch (e: Exception) {
            // WHY: Hardware keystore failure is non-fatal. The Rust FileKeystore
            // still works as a fallback (plaintext file in app-private storage).
            // Logging at ERROR level so this shows up in diagnostics — it means
            // the device has no hardware key protection, which is a security
            // concern but not a functional blocker.
            Log.e(TAG, "HardwareKeystore initialization failed: ${e.message}. " +
                "Wallet keys will use software-only storage.", e)
        }
    }
}
