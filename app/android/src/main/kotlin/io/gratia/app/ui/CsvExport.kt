package io.gratia.app.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import androidx.core.content.FileProvider
import java.io.File
import java.io.FileWriter
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Utility for exporting transaction history to CSV and sharing via Android's
 * share sheet. Uses FileProvider for secure file sharing across app boundaries.
 */
object CsvExporter {

    private const val TAG = "CsvExporter"

    // WHY: Subdirectory inside cache — keeps exports isolated so they can be
    // cleaned up independently without wiping other cached data.
    private const val EXPORT_DIR = "csv_exports"

    // WHY: ISO 8601 for the CSV cell values — universally parseable by
    // spreadsheet software and unambiguous across locales.
    private const val DATE_FORMAT_CSV = "yyyy-MM-dd HH:mm:ss"

    // WHY: Compact timestamp for the filename — avoids spaces and colons
    // that cause issues on some filesystems and share targets.
    private const val DATE_FORMAT_FILENAME = "yyyyMMdd_HHmmss"

    // 1 GRAT = 1,000,000 Lux (defined in tokenomics)
    private const val LUX_PER_GRAT = 1_000_000.0

    /**
     * Exports the given transactions to a CSV file in the app's cache directory.
     *
     * @param context Android context for accessing the cache directory and FileProvider.
     * @param transactions The list of transactions to export.
     * @return A content:// [Uri] suitable for sharing, or null if export failed.
     */
    fun exportTransactions(context: Context, transactions: List<TransactionInfo>): Uri? {
        return try {
            val exportDir = File(context.cacheDir, EXPORT_DIR)
            if (!exportDir.exists()) {
                exportDir.mkdirs()
            }

            val timestamp = SimpleDateFormat(DATE_FORMAT_FILENAME, Locale.US).format(Date())
            val file = File(exportDir, "gratia_transactions_$timestamp.csv")

            val dateFormatter = SimpleDateFormat(DATE_FORMAT_CSV, Locale.getDefault())

            FileWriter(file).use { writer ->
                // Header row
                writer.append("Date,Type,From,To,Amount (GRAT),Amount (Lux),TX Hash,Block Height\n")

                for (tx in transactions) {
                    val date = dateFormatter.format(Date(tx.timestampMillis))
                    val type = tx.direction
                    val from = if (tx.direction == "sent") "self" else (tx.counterparty ?: "unknown")
                    val to = if (tx.direction == "sent") (tx.counterparty ?: "unknown") else "self"
                    val amountGrat = tx.amountLux / LUX_PER_GRAT
                    val amountLux = tx.amountLux

                    // WHY: Wrap fields in quotes to handle any commas or special
                    // characters in addresses or hashes (defensive formatting).
                    writer.append(
                        "\"${escapeCsv(date)}\"," +
                        "\"${escapeCsv(type)}\"," +
                        "\"${escapeCsv(from)}\"," +
                        "\"${escapeCsv(to)}\"," +
                        "\"${"%.6f".format(amountGrat)}\"," +
                        "\"$amountLux\"," +
                        "\"${escapeCsv(tx.hashHex)}\"," +
                        // WHY: Block height is not yet available in TransactionInfo.
                        // Placeholder until the data class is extended with on-chain metadata.
                        "\"N/A\"\n"
                    )
                }
            }

            // WHY: FileProvider creates a content:// URI with temporary read permission.
            // Direct file:// URIs throw FileUriExposedException on Android 7.0+.
            FileProvider.getUriForFile(
                context,
                "${context.packageName}.fileprovider",
                file
            )
        } catch (e: Exception) {
            Log.e(TAG, "Failed to export transactions to CSV", e)
            null
        }
    }

    /**
     * Exports transactions to CSV and opens the Android share sheet so the user
     * can send the file via email, messaging apps, cloud storage, etc.
     *
     * @param context Android context for creating the share intent.
     * @param transactions The list of transactions to export and share.
     */
    fun shareTransactions(context: Context, transactions: List<TransactionInfo>) {
        val uri = exportTransactions(context, transactions)
        if (uri == null) {
            Log.e(TAG, "Cannot share — CSV export failed")
            return
        }

        val shareIntent = Intent(Intent.ACTION_SEND).apply {
            type = "text/csv"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, "Gratia Transaction History")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }

        val chooser = Intent.createChooser(shareIntent, "Share transaction history")
        // WHY: FLAG_ACTIVITY_NEW_TASK is required when starting an activity from
        // a non-Activity context (e.g., from a ViewModel or service).
        chooser.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(chooser)
    }

    /**
     * Escapes a string for safe inclusion in a quoted CSV field.
     * Doubles any embedded quote characters per RFC 4180.
     */
    private fun escapeCsv(value: String): String {
        return value.replace("\"", "\"\"")
    }
}
