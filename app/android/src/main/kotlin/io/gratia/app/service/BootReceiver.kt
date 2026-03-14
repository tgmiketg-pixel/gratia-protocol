package io.gratia.app.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log

/**
 * Starts the Proof of Life foreground service after device boot.
 *
 * Registered in AndroidManifest.xml with:
 * ```xml
 * <receiver android:name=".service.BootReceiver"
 *           android:exported="false"
 *           android:enabled="true">
 *     <intent-filter>
 *         <action android:name="android.intent.action.BOOT_COMPLETED" />
 *     </intent-filter>
 * </receiver>
 * ```
 *
 * WHY: Proof of Life data collection must begin as early as possible after
 * a reboot. Missing hours of sensor data could cause a day's PoL to fail,
 * locking the user out of mining. Starting on boot ensures the full
 * rolling 24-hour window is covered.
 *
 * The MiningService is NOT started here. Mining activation is driven by
 * power-state events (plugged in + battery >= 80%), which the
 * ProofOfLifeService monitors and delegates to MiningService when
 * conditions are met.
 */
class BootReceiver : BroadcastReceiver() {

    companion object {
        private const val TAG = "GratiaBootReceiver"
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return

        Log.i(TAG, "Boot completed — starting ProofOfLifeService")

        val serviceIntent = Intent(context, ProofOfLifeService::class.java)

        // WHY: On Android 8.0+ (API 26+), background-started services must be
        // foreground services. startForegroundService() gives the service a
        // short window (5 seconds on most OEMs) to call startForeground().
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(serviceIntent)
        } else {
            context.startService(serviceIntent)
        }
    }
}
