package io.gratia.app.service

import android.nfc.cardemulation.HostApduService
import android.os.Bundle
import android.util.Log
import io.gratia.app.bridge.GratiaCoreManager

/**
 * Host Card Emulation (HCE) service for NFC tap-to-transact.
 *
 * When another phone's NFC reader selects this service via the Gratia AID,
 * it responds with the local wallet address as UTF-8 bytes. The reader phone
 * then opens its send dialog pre-filled with this address.
 *
 * This runs as a system service — Android routes incoming NFC APDUs to it
 * automatically when the Gratia AID is selected, even without the app in
 * the foreground.
 */
class NfcTransactService : HostApduService() {

    companion object {
        private const val TAG = "GratiaNfcHCE"

        // WHY: ISO 7816-4 status words. These are standard APDU response codes
        // that NFC readers expect. Non-standard codes may cause reader-side errors.
        private val SW_OK = byteArrayOf(0x90.toByte(), 0x00.toByte())
        private val SW_UNKNOWN = byteArrayOf(0x6F.toByte(), 0x00.toByte())

        // WHY: SELECT APDU header for matching incoming requests.
        // CLA=0x00, INS=0xA4 (SELECT), P1=0x04 (by name), P2=0x00.
        private val SELECT_HEADER = byteArrayOf(
            0x00.toByte(), // CLA
            0xA4.toByte(), // INS: SELECT
            0x04.toByte(), // P1: select by name
            0x00.toByte(), // P2
        )

        // WHY: Custom AID F0475241544941 = F0 prefix (proprietary) + "GRATIA" in ASCII.
        // This avoids collision with registered payment AIDs (A0 prefix per ISO 7816-5).
        private val GRATIA_AID = byteArrayOf(
            0xF0.toByte(), 0x47, 0x52, 0x41, 0x54, 0x49, 0x41
        )
    }

    /**
     * Called when an NFC reader sends an APDU command to this service.
     *
     * We only respond to SELECT commands with our AID. Everything else
     * gets an error response.
     */
    override fun processCommandApdu(commandApdu: ByteArray, extras: Bundle?): ByteArray {
        Log.d(TAG, "Received APDU: ${bytesToHex(commandApdu)}")

        if (!isSelectApdu(commandApdu)) {
            Log.w(TAG, "Received non-SELECT APDU, returning error")
            return SW_UNKNOWN
        }

        // WHY: Check that the Rust core is initialized before accessing the wallet.
        // On fresh installs where no wallet exists yet, we return an error rather
        // than crashing.
        if (!GratiaCoreManager.isInitialized) {
            Log.w(TAG, "Rust core not initialized, cannot serve wallet address")
            return SW_UNKNOWN
        }

        return try {
            val walletInfo = GratiaCoreManager.getWalletInfo()
            val addressBytes = walletInfo.address.toByteArray(Charsets.UTF_8)
            Log.i(TAG, "Serving wallet address via NFC HCE (${walletInfo.address.take(12)}...)")

            // WHY: APDU response format is payload + status word. The reader
            // strips the trailing 2-byte SW and reads the remaining bytes as
            // the wallet address string.
            addressBytes + SW_OK
        } catch (e: Exception) {
            Log.e(TAG, "Failed to get wallet address for NFC response", e)
            SW_UNKNOWN
        }
    }

    /**
     * Called when the NFC link is lost or deactivated.
     *
     * @param reason One of [DEACTIVATION_LINK_LOSS] or [DEACTIVATION_DESELECTED].
     */
    override fun onDeactivated(reason: Int) {
        val reasonStr = when (reason) {
            DEACTIVATION_LINK_LOSS -> "link loss"
            DEACTIVATION_DESELECTED -> "deselected"
            else -> "unknown ($reason)"
        }
        Log.d(TAG, "NFC HCE deactivated: $reasonStr")
    }

    /**
     * Check if the incoming APDU is a SELECT command for our AID.
     *
     * SELECT APDU format:
     * [CLA=00][INS=A4][P1=04][P2=00][Lc=07][AID: F0475241544941][Le=00]
     */
    private fun isSelectApdu(apdu: ByteArray): Boolean {
        // WHY: Minimum length is 4 (header) + 1 (Lc) + 7 (AID) = 12.
        // Le byte at the end is optional, so we check >= 12.
        if (apdu.size < 12) return false

        // Check CLA, INS, P1, P2 header
        for (i in SELECT_HEADER.indices) {
            if (apdu[i] != SELECT_HEADER[i]) return false
        }

        // Check Lc (AID length)
        val aidLength = apdu[4].toInt() and 0xFF
        if (aidLength != GRATIA_AID.size) return false

        // Check AID bytes
        for (i in GRATIA_AID.indices) {
            if (apdu[5 + i] != GRATIA_AID[i]) return false
        }

        return true
    }

    private fun bytesToHex(bytes: ByteArray): String {
        return bytes.joinToString("") { "%02X".format(it) }
    }
}
