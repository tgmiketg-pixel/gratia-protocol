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
 *           android:exported="true"
 *           android:directBootAware="true">
 *     <intent-filter>
 *         <action android:name="android.intent.action.BOOT_COMPLETED" />
 *         <action android:name="android.intent.action.QUICKBOOT_POWERON" />
 *     </intent-filter>
 * </receiver>
 * ```
 *
 * WHY: Proof of Life data collection must begin as early as possible after
 * a reboot. Missing hours of sensor data could cause a day's PoL to fail,
 * locking the user out of mining. Starting on boot ensures the full
 * rolling 24-hour window is covered.
 *
 * WHY directBootAware: On Android 7.0+ (API 24+), the device can be in a
 * "Direct Boot" state after reboot before the user unlocks for the first
 * time. With directBootAware="true", this receiver fires immediately at
 * boot rather than waiting for the user to unlock, which can be hours
 * later. This maximizes the PoL data collection window.
 *
 * WHY QUICKBOOT_POWERON: Some OEMs (HTC, Samsung, and others) send
 * com.htc.intent.action.QUICKBOOT_POWERON instead of or in addition to
 * BOOT_COMPLETED when the device resumes from a fast-boot or hibernation
 * state. Listening to both ensures PoL starts on all devices.
 *
 * The MiningService is NOT started here. Mining activation is driven by
 * power-state events (plugged in + battery >= 80%), which the
 * ProofOfLifeService monitors and delegates to MiningService when
 * conditions are met.
 */
class BootReceiver : BroadcastReceiver() {

    companion object {
        private const val TAG = "GratiaBootReceiver"

        // WHY: These are the two boot actions we handle. BOOT_COMPLETED is the
        // standard Android broadcast; QUICKBOOT_POWERON covers HTC/Samsung
        // fast-boot scenarios where BOOT_COMPLETED may not fire.
        private const val ACTION_QUICKBOOT = "android.intent.action.QUICKBOOT_POWERON"
    }

    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action

        if (action != Intent.ACTION_BOOT_COMPLETED && action != ACTION_QUICKBOOT) {
            Log.w(TAG, "Received unexpected action: $action — ignoring")
            return
        }

        Log.i(TAG, "Boot receiver fired — action=$action, starting ProofOfLifeService")

        val serviceIntent = Intent(context, ProofOfLifeService::class.java)

        // WHY: On Android 12+ (SDK 31+), background-started foreground services
        // face additional restrictions. startForegroundService() is required on
        // Android 8.0+ (API 26+) and gives the service a short window (~10
        // seconds) to call startForeground(). Boot-completed receivers are
        // explicitly exempted from the Android 12 background start restrictions,
        // so startForegroundService() works reliably here.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(serviceIntent)
        } else {
            context.startService(serviceIntent)
        }

        Log.i(TAG, "ProofOfLifeService start requested successfully")
    }
}
