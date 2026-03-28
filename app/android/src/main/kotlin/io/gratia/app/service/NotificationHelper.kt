package io.gratia.app.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat

/**
 * Centralized notification channel setup and notification builders for Gratia services.
 *
 * Creates three notification channels:
 * - proof_of_life: Silent, low-importance channel for the PoL foreground service.
 * - mining: Default-importance channel for the mining foreground service.
 * - transactions: High-importance channel for received payment alerts.
 */
object NotificationHelper {

    // -- Channel IDs -------------------------------------------------------

    // WHY: Channel ID "proof_of_life_v2" replaces the old "proof_of_life" which
    // was created at IMPORTANCE_LOW. Android won't upgrade importance on an
    // existing channel, and deleting a channel while a foreground service uses
    // it crashes (enforceDeletingChannelHasNoFgService). A new ID sidesteps both.
    const val CHANNEL_PROOF_OF_LIFE = "proof_of_life_v2"
    // WHY: "mining_v2" replaces old "mining" channel that had sound/vibration.
    // Can't delete old channel while foreground service uses it (crashes on Samsung).
    const val CHANNEL_MINING = "mining_v2"
    const val CHANNEL_TRANSACTIONS = "transactions"

    // -- Notification IDs --------------------------------------------------
    // WHY: Stable IDs so startForeground() can update the same notification
    // without creating duplicates.

    const val NOTIFICATION_ID_POL = 1001
    const val NOTIFICATION_ID_MINING = 1002

    // -- Channel Creation --------------------------------------------------

    /**
     * Create all notification channels. Safe to call multiple times;
     * the system ignores duplicate channel creation.
     *
     * Must be called before posting any notification, ideally at Application.onCreate().
     */
    fun createChannels(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return

        val manager = context.getSystemService(NotificationManager::class.java) ?: return

        // WHY: DEFAULT importance (not LOW) for PoL so the status bar icon is
        // visible on Samsung/One UI. LOW importance hides the icon entirely,
        // making users think the app isn't running. We keep it silent (no sound,
        // no vibration) so it doesn't annoy — but the icon must be visible.
        val polChannel = NotificationChannel(
            CHANNEL_PROOF_OF_LIFE,
            "Proof of Life",
            NotificationManager.IMPORTANCE_DEFAULT
        ).apply {
            description = "Background activity verification for Gratia"
            setShowBadge(false)
            // WHY: No sound, no vibration — this is a silent persistent notification.
            setSound(null, null)
            enableVibration(false)
        }

        val miningChannel = NotificationChannel(
            CHANNEL_MINING,
            "Mining",
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Active GRAT mining status"
            setShowBadge(false)
            // WHY: Mining runs for hours. Sound/vibration on every notification
            // update would drive users insane. Silent + visible icon only.
            setSound(null, null)
            enableVibration(false)
        }

        // WHY: HIGH importance for transactions — the user wants to know immediately
        // when they receive GRAT, similar to a payment notification.
        val txChannel = NotificationChannel(
            CHANNEL_TRANSACTIONS,
            "Transactions",
            NotificationManager.IMPORTANCE_HIGH
        ).apply {
            description = "Incoming GRAT payment notifications"
            setShowBadge(true)
            enableVibration(true)
        }

        manager.createNotificationChannels(listOf(polChannel, miningChannel, txChannel))
    }

    // -- Notification Builders ---------------------------------------------

    /**
     * Build the persistent foreground notification for the Proof of Life service.
     *
     * This notification is silent and low-priority. It satisfies Android's
     * foreground service requirement without bothering the user.
     */
    fun buildProofOfLifeNotification(
        context: Context,
        contentText: String = "Gratia is verifying your activity"
    ): Notification {
        val launchIntent = context.packageManager
            .getLaunchIntentForPackage(context.packageName)
            ?.apply { flags = Intent.FLAG_ACTIVITY_SINGLE_TOP }

        val pendingIntent = launchIntent?.let {
            PendingIntent.getActivity(
                context, 0, it,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
        }

        return NotificationCompat.Builder(context, CHANNEL_PROOF_OF_LIFE)
            .setContentTitle("Gratia")
            .setContentText(contentText)
            .setSmallIcon(io.gratia.app.R.drawable.ic_notification)
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .apply { pendingIntent?.let { setContentIntent(it) } }
            .build()
    }

    /**
     * Build the foreground notification for the mining service.
     *
     * Shows current mining status and earnings rate. Updated periodically
     * while mining is active.
     */
    fun buildMiningNotification(
        context: Context,
        earningsPerHour: String = "calculating..."
    ): Notification {
        val launchIntent = context.packageManager
            .getLaunchIntentForPackage(context.packageName)
            ?.apply { flags = Intent.FLAG_ACTIVITY_SINGLE_TOP }

        val pendingIntent = launchIntent?.let {
            PendingIntent.getActivity(
                context, 0, it,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
        }

        return NotificationCompat.Builder(context, CHANNEL_MINING)
            .setContentTitle("Mining GRAT")
            .setContentText("Mining GRAT \u2014 $earningsPerHour")
            .setSmallIcon(io.gratia.app.R.drawable.ic_notification)
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .apply { pendingIntent?.let { setContentIntent(it) } }
            .build()
    }

    /**
     * Build a notification for a received GRAT transaction.
     */
    fun buildTransactionNotification(
        context: Context,
        amountGrat: String,
        senderAddress: String
    ): Notification {
        val launchIntent = context.packageManager
            .getLaunchIntentForPackage(context.packageName)
            ?.apply { flags = Intent.FLAG_ACTIVITY_SINGLE_TOP }

        val pendingIntent = launchIntent?.let {
            PendingIntent.getActivity(
                context, 0, it,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
        }

        // WHY: Show truncated sender address (first 10 + last 4 hex chars) for
        // readability. Full address is visible in the app.
        val shortSender = if (senderAddress.length > 20) {
            "${senderAddress.take(14)}...${senderAddress.takeLast(4)}"
        } else {
            senderAddress
        }

        return NotificationCompat.Builder(context, CHANNEL_TRANSACTIONS)
            .setContentTitle("Received $amountGrat GRAT")
            .setContentText("From $shortSender")
            .setSmallIcon(io.gratia.app.R.drawable.ic_notification)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .apply { pendingIntent?.let { setContentIntent(it) } }
            .build()
    }
}
