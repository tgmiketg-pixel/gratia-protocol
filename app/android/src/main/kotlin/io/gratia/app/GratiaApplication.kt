package io.gratia.app

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import io.gratia.app.BuildConfig
import io.gratia.app.bridge.GratiaCoreManager
import io.gratia.app.bridge.GratiaBridgeException
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
    }

    override fun onCreate() {
        super.onCreate()

        Log.i(TAG, "Gratia application starting")

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
     * Uses port 0 so the OS assigns an available UDP port — avoids conflicts
     * when multiple Gratia instances run on the same LAN during testing.
     */
    private fun startP2PNetwork() {
        if (!GratiaCoreManager.isInitialized) {
            Log.w(TAG, "Skipping P2P network start — core not initialized")
            return
        }

        try {
            // WHY: Fixed port 9000 for the demo so phones can connect to each other
            // at a known address. Each phone has a different IP on the LAN so port
            // collisions don't occur. In production, use port 0 (OS-assigned).
            val status = GratiaCoreManager.startNetwork(listenPort = 9000)
            Log.i(TAG, "P2P network started — listening on: ${status.listenAddress ?: "unknown"}, peers: ${status.peerCount}")

            // WHY: Staged startup sequence. Each component waits for the
            // previous one to stabilize before starting:
            //
            //   t=0s   Network starts (above) — UDP listener + mDNS begin
            //   t=10s  Explorer API — serves chain data, doesn't need peers
            //   t=10s  GratiaVM — deploys demo contracts, doesn't need peers
            //   t=15s  Consensus — by now mDNS has discovered peers (1-2s),
            //          gossipsub mesh has formed (~10s heartbeat on mobile),
            //          and NodeAnnouncements have propagated. Starting consensus
            //          here means the committee is built with real peer data
            //          instead of only synthetic padding nodes.
            //
            // WHY 15 seconds for consensus: mDNS discovery takes 1-2s, but the
            // gossipsub mesh needs at least one heartbeat cycle (configured at
            // 30s, but initial subscription propagation is faster). 15s gives
            // enough time for peer discovery + gossip subscription + node
            // announcement exchange, while still feeling responsive to the user.
            Thread {
                Thread.sleep(10_000)

                // Explorer API — lightweight, no peer dependency
                try {
                    val url = GratiaCoreManager.startExplorerApi(8080)
                    Log.i(TAG, "Explorer API started: $url")
                } catch (e: Exception) {
                    Log.w(TAG, "Explorer API start failed: ${e.message}")
                }

                // GratiaVM — deploys demo contracts
                try {
                    val contracts = GratiaCoreManager.initVm()
                    Log.i(TAG, "GratiaVM initialized: ${contracts.size} contracts deployed")
                } catch (e: Exception) {
                    Log.w(TAG, "GratiaVM init failed: ${e.message}")
                }

                // Wait for gossipsub mesh to form before starting consensus
                Thread.sleep(5_000)

                try {
                    val consensusStatus = GratiaCoreManager.startConsensus()
                    Log.i(TAG, "Consensus started — slot: ${consensusStatus.currentSlot}, committee: ${consensusStatus.isCommitteeMember}")
                } catch (e: Exception) {
                    Log.w(TAG, "Consensus start failed (may already be running): ${e.message}")
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

            // Auto-create a wallet on first launch if one doesn't exist.
            // WHY: The consensus engine needs a signing key (derived from the wallet)
            // for VRF block producer selection. Without a wallet, consensus can't start.
            // This matches the onboarding design: "install, use phone normally" — no
            // manual wallet creation step required.
            //
            // WHY: FileKeystore now auto-loads the key from disk if it exists, so
            // getWalletInfo() succeeding means we already have a wallet. We only
            // call createWallet() when no key file was found.
            try {
                val info = GratiaCoreManager.getWalletInfo()
                Log.i(TAG, "Wallet loaded from file: ${info.address}")
            } catch (e: Exception) {
                // No existing wallet — create a new one
                try {
                    val address = GratiaCoreManager.createWallet()
                    Log.i(TAG, "Wallet created: $address")
                } catch (e2: Exception) {
                    Log.e(TAG, "Failed to create wallet: ${e2.message}")
                }
            }
            // WHY: Auto-start networking and consensus on launch so the user
            // never has to manually navigate to the Network tab. This matches
            // the "install, plug in, mine" zero-delay onboarding design.
            // Network connects to the bootstrap node, consensus starts
            // producing/validating blocks immediately.
            try {
                GratiaCoreManager.startNetwork(9000)
                Log.i(TAG, "Network auto-started on port 9000")
            } catch (e: Exception) {
                Log.w(TAG, "Failed to auto-start network: ${e.message}")
            }

            try {
                GratiaCoreManager.startConsensus()
                Log.i(TAG, "Consensus auto-started")
            } catch (e: Exception) {
                Log.w(TAG, "Failed to auto-start consensus: ${e.message}")
            }

        } catch (e: Exception) {
            // WHY: We log but don't crash here. The app can still display the UI
            // and will show appropriate error states. This handles the case where
            // the native .so library isn't loaded yet during development.
            Log.e(TAG, "Failed to initialize Rust core: ${e.message}", e)
        }
    }
}
