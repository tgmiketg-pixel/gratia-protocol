package io.gratia.app.worker

import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import io.gratia.app.service.ProofOfLifeService

/**
 * WorkManager periodic worker that acts as a backup heartbeat for
 * [ProofOfLifeService].
 *
 * WHY: Android can kill foreground services under memory pressure or after
 * extended background time (especially on OEM-skinned Android builds like
 * MIUI, OneUI, ColorOS). START_STICKY requests a restart, but the system
 * does not guarantee it. WorkManager provides a second, independent
 * mechanism to detect a dead service and restart it. This ensures PoL
 * data collection continues even on aggressive OEM devices.
 *
 * Runs every 15 minutes (WorkManager minimum periodic interval).
 */
class PolHeartbeatWorker(
    appContext: Context,
    workerParams: WorkerParameters
) : CoroutineWorker(appContext, workerParams) {

    companion object {
        private const val TAG = "PolHeartbeatWorker"

        /** Unique work name used for enqueuing — must match the name in GratiaApplication. */
        const val WORK_NAME = "pol_heartbeat"
    }

    override suspend fun doWork(): Result {
        val isRunning = isServiceRunning(ProofOfLifeService::class.java)

        if (isRunning) {
            Log.d(TAG, "Heartbeat check: ProofOfLifeService is running")
        } else {
            Log.w(TAG, "Heartbeat check: ProofOfLifeService is NOT running — restarting")
            try {
                val intent = Intent(applicationContext, ProofOfLifeService::class.java)
                applicationContext.startForegroundService(intent)
                Log.i(TAG, "ProofOfLifeService restart requested via startForegroundService")
            } catch (e: Exception) {
                Log.e(TAG, "Failed to restart ProofOfLifeService: ${e.message}", e)
            }
        }

        return Result.success()
    }

    /**
     * Check whether a service class is currently running.
     *
     * WHY: Uses ActivityManager.getRunningServices() which is deprecated for
     * third-party app discovery but still works reliably for checking your own
     * app's services. There is no modern replacement for this use case.
     */
    @Suppress("DEPRECATION")
    private fun isServiceRunning(serviceClass: Class<*>): Boolean {
        val activityManager = applicationContext.getSystemService(Context.ACTIVITY_SERVICE)
            as ActivityManager
        // WHY: maxNum=50 is generous — our app runs at most 2 services (PoL + Mining).
        // A smaller number risks missing the service in the list on devices with many
        // background services.
        for (service in activityManager.getRunningServices(50)) {
            if (serviceClass.name == service.service.className) {
                return true
            }
        }
        return false
    }
}
