package io.gratia.app.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import io.gratia.app.MainActivity
import io.gratia.app.R
import java.util.Locale

/**
 * Home screen widget that displays current GRAT balance and mining status.
 *
 * Reads persisted state from SharedPreferences written by MiningService:
 * - "persisted_balance_lux" (Long): balance in Lux (1 GRAT = 1,000,000 Lux)
 * - "is_mining" (Boolean): whether the device is currently mining
 *
 * Updates every 30 minutes via updatePeriodMillis in widget metadata.
 */
class GratiaWidget : AppWidgetProvider() {

    override fun onUpdate(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetIds: IntArray
    ) {
        for (appWidgetId in appWidgetIds) {
            updateAppWidget(context, appWidgetManager, appWidgetId)
        }
    }

    override fun onEnabled(context: Context) {
        // No-op: nothing special needed when the first widget is placed.
    }

    override fun onDisabled(context: Context) {
        // No-op: nothing special needed when the last widget is removed.
    }

    companion object {
        // WHY: Must match the SharedPreferences name and keys used by MiningService
        // so the widget reads the same persisted state the service writes.
        private const val PREFS_NAME = "gratia_mining_prefs"
        private const val KEY_BALANCE_LUX = "persisted_balance_lux"
        private const val KEY_IS_MINING = "is_mining"

        // 1 GRAT = 1,000,000 Lux (the smallest unit of GRAT)
        private const val LUX_PER_GRAT = 1_000_000.0

        /**
         * Updates a single widget instance with current balance and mining status.
         * Called from onUpdate and can also be called externally when state changes
         * (e.g., from MiningService) to keep the widget fresh between 30-min cycles.
         */
        fun updateAppWidget(
            context: Context,
            appWidgetManager: AppWidgetManager,
            appWidgetId: Int
        ) {
            val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            val balanceLux = prefs.getLong(KEY_BALANCE_LUX, 0L)
            val isMining = prefs.getBoolean(KEY_IS_MINING, false)

            val balanceGrat = balanceLux / LUX_PER_GRAT
            val balanceText = String.format(Locale.US, "%.2f GRAT", balanceGrat)
            val statusText = if (isMining) "Mining" else "Not Mining"

            val views = RemoteViews(context.packageName, R.layout.widget_gratia).apply {
                setTextViewText(R.id.widget_balance, balanceText)
                setTextViewText(R.id.widget_status, statusText)
            }

            // WHY: FLAG_IMMUTABLE is required on Android 12+ for PendingIntents that
            // don't need to be modified by the receiver. FLAG_UPDATE_CURRENT ensures
            // existing intents are updated rather than duplicated.
            val intent = Intent(context, MainActivity::class.java)
            val pendingIntent = PendingIntent.getActivity(
                context,
                0,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            views.setOnClickPendingIntent(R.id.widget_root, pendingIntent)

            appWidgetManager.updateAppWidget(appWidgetId, views)
        }
    }
}
