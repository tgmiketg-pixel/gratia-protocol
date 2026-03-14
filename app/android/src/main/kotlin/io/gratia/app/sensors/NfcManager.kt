package io.gratia.app.sensors

import android.app.Activity
import android.content.Context
import android.nfc.NdefMessage
import android.nfc.NdefRecord
import android.nfc.NfcAdapter
import android.nfc.Tag
import android.os.Bundle
import android.util.Log

/**
 * NFC manager for future tap-to-transact functionality.
 *
 * Currently provides NFC availability detection and basic scaffolding for
 * NDEF message handling. The full tap-to-transact protocol (encoding a
 * transaction request in an NDEF record, reading it on the receiver's phone,
 * and confirming via the Rust wallet layer) will be implemented in Phase 1
 * milestone 9.
 *
 * NFC is an optional sensor that contributes +5 to the Composite Presence
 * Score but is not required for PoL threshold.
 */
class NfcManager(
    private val context: Context
) {

    companion object {
        private const val TAG = "GratiaNfc"
    }

    private val nfcAdapter: NfcAdapter? = NfcAdapter.getDefaultAdapter(context)
    private var isRunning = false

    /**
     * Callback for when an NFC tag is discovered.
     *
     * Will be used for tap-to-transact: the tag contains a Gratia payment
     * request (address + amount) encoded as an NDEF record.
     */
    var onTagDiscovered: ((tag: Tag) -> Unit)? = null

    /**
     * Callback for when an NDEF message is received via Android Beam
     * or Host Card Emulation (HCE).
     */
    var onNdefMessageReceived: ((message: NdefMessage) -> Unit)? = null

    /**
     * Start NFC detection.
     *
     * Note: Full NFC foreground dispatch requires an Activity reference.
     * This start() only verifies availability. Call [enableForegroundDispatch]
     * from the active Activity to actually intercept NFC intents.
     */
    fun start() {
        if (isRunning) return

        if (nfcAdapter == null) {
            Log.d(TAG, "NFC not available on this device")
            return
        }

        if (!nfcAdapter.isEnabled) {
            Log.w(TAG, "NFC is disabled in system settings")
            return
        }

        isRunning = true
        Log.i(TAG, "NFC manager started (adapter available and enabled)")
    }

    /** Stop NFC detection. */
    fun stop() {
        if (!isRunning) return
        isRunning = false
        Log.i(TAG, "NFC manager stopped")
    }

    /** Check whether this manager is active. */
    fun isActive(): Boolean = isRunning

    /** Check whether NFC hardware is present on this device. */
    fun isAvailable(): Boolean = nfcAdapter != null

    /** Check whether NFC is currently enabled in system settings. */
    fun isEnabled(): Boolean = nfcAdapter?.isEnabled == true

    /**
     * Enable foreground dispatch for NFC tag discovery.
     *
     * Must be called from [Activity.onResume]. This gives the Gratia app
     * priority over other apps for handling NFC tags while in the foreground.
     *
     * @param activity The currently active Activity.
     */
    fun enableForegroundDispatch(activity: Activity) {
        if (nfcAdapter == null || !nfcAdapter.isEnabled) return

        try {
            val intent = android.content.Intent(context, activity.javaClass).apply {
                addFlags(android.content.Intent.FLAG_ACTIVITY_SINGLE_TOP)
            }
            val pendingIntent = android.app.PendingIntent.getActivity(
                context,
                0,
                intent,
                android.app.PendingIntent.FLAG_MUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT
            )

            // WHY: We filter for NDEF messages containing our custom MIME type.
            // This prevents the app from intercepting unrelated NFC tags.
            val filters = arrayOf(
                android.content.IntentFilter(NfcAdapter.ACTION_NDEF_DISCOVERED).apply {
                    try {
                        addDataType("application/io.gratia.transaction")
                    } catch (e: android.content.IntentFilter.MalformedMimeTypeException) {
                        Log.e(TAG, "Malformed MIME type", e)
                    }
                }
            )

            nfcAdapter.enableForegroundDispatch(activity, pendingIntent, filters, null)
            Log.d(TAG, "NFC foreground dispatch enabled")
        } catch (e: Exception) {
            Log.w(TAG, "Failed to enable NFC foreground dispatch: ${e.message}")
        }
    }

    /**
     * Disable foreground dispatch.
     *
     * Must be called from [Activity.onPause].
     *
     * @param activity The currently active Activity.
     */
    fun disableForegroundDispatch(activity: Activity) {
        if (nfcAdapter == null) return

        try {
            nfcAdapter.disableForegroundDispatch(activity)
            Log.d(TAG, "NFC foreground dispatch disabled")
        } catch (e: IllegalStateException) {
            // Activity may not be in the foreground.
            Log.d(TAG, "Could not disable foreground dispatch: ${e.message}")
        }
    }

    /**
     * Process an NFC intent received by the Activity.
     *
     * Call this from [Activity.onNewIntent] when the intent has an NFC action.
     *
     * @param intent The intent containing NFC data.
     */
    fun handleIntent(intent: android.content.Intent) {
        when (intent.action) {
            NfcAdapter.ACTION_NDEF_DISCOVERED,
            NfcAdapter.ACTION_TECH_DISCOVERED,
            NfcAdapter.ACTION_TAG_DISCOVERED -> {
                val tag = intent.getParcelableExtra<Tag>(NfcAdapter.EXTRA_TAG)
                tag?.let {
                    Log.d(TAG, "NFC tag discovered: ${it.id?.let { id -> bytesToHex(id) }}")
                    onTagDiscovered?.invoke(it)
                }

                // Extract NDEF messages if present.
                val rawMessages = intent.getParcelableArrayExtra(NfcAdapter.EXTRA_NDEF_MESSAGES)
                rawMessages?.filterIsInstance<NdefMessage>()?.firstOrNull()?.let { message ->
                    Log.d(TAG, "NDEF message received (${message.records.size} records)")
                    onNdefMessageReceived?.invoke(message)
                }
            }
        }
    }

    /**
     * Create a Gratia payment request NDEF record.
     *
     * This will be used by the payee to create an NFC tag that the payer
     * taps to initiate a transaction.
     *
     * @param address The recipient address (grat:<hex>).
     * @param amountLux The requested amount in Lux (0 for open-ended).
     * @return An NdefRecord encoding the payment request.
     */
    fun createPaymentRequestRecord(address: String, amountLux: Long): NdefRecord {
        // WHY: Custom MIME type allows filtering NFC intents to only Gratia
        // payment requests. The payload is a simple "address|amount" string
        // for now — will be replaced with a protobuf or CBOR encoding in
        // the production implementation.
        val payload = "$address|$amountLux"
        return NdefRecord.createMime(
            "application/io.gratia.transaction",
            payload.toByteArray(Charsets.UTF_8)
        )
    }

    // ========================================================================
    // Internal
    // ========================================================================

    private fun bytesToHex(bytes: ByteArray): String {
        return bytes.joinToString("") { "%02x".format(it) }
    }
}
