package io.gratia.app.security

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject

/**
 * Contact entry in the address book.
 *
 * @property name Human-readable label chosen by the user (e.g., "Mom", "Alice").
 * @property address Full Gratia wallet address including the "grat:" prefix.
 * @property addedAtMillis Epoch millis when this contact was saved, used for
 *   display ordering and future export/sync features.
 */
data class Contact(
    val name: String,
    val address: String,
    val addedAtMillis: Long
)

/**
 * Local address book that stores name-to-wallet-address mappings.
 *
 * WHY: Users send GRAT to the same people repeatedly. Requiring them to
 * paste or scan a 69-character address every time is error-prone and hostile
 * UX. A simple contacts list with nicknames eliminates mistyped addresses
 * and makes the wallet feel like a normal payment app.
 *
 * Data is persisted in SharedPreferences as a JSON array. This is acceptable
 * because the dataset is small (max 100 contacts, ~10 KB) and does not
 * warrant a full Room/SQLite database. If the address book grows in scope
 * (e.g., contact avatars, transaction history per contact), migrate to Room.
 *
 * Privacy note: contacts are stored on-device only. They are never synced
 * to any server or included in any attestation.
 */
object AddressBook {

    private const val TAG = "GratiaAddressBook"

    private const val PREFS_FILE = "gratia_address_book"
    private const val KEY_CONTACTS = "contacts"

    // WHY: 69 chars = 5-char prefix "grat:" + 64-char hex-encoded Ed25519 public key.
    // This mirrors the address format produced by gratia-wallet's key generation.
    private const val VALID_ADDRESS_LENGTH = 69
    private const val ADDRESS_PREFIX = "grat:"

    // WHY: 100 contacts is generous for a mobile wallet. Unbounded lists risk
    // OOM on low-end devices during JSON parse and make the UI scroll unusable.
    // Can be raised via governance or app update if users hit this in practice.
    private const val MAX_CONTACTS = 100

    private lateinit var prefs: SharedPreferences
    private val contacts = mutableListOf<Contact>()

    /**
     * Initialize the address book. Must be called once during app startup
     * (typically in Application.onCreate or the main Activity) before any
     * other method is used.
     *
     * @param context Application context (not Activity — avoids leaks).
     */
    fun init(context: Context) {
        prefs = context.applicationContext.getSharedPreferences(PREFS_FILE, Context.MODE_PRIVATE)
        loadFromDisk()
        Log.d(TAG, "Address book initialized with ${contacts.size} contacts")
    }

    /**
     * Add a new contact. Validates the address format and enforces the cap.
     *
     * @param name Display name for the contact. Must not be blank.
     * @param address Gratia wallet address (must be 69 chars, "grat:" prefix).
     * @return true if the contact was added, false if validation failed or
     *   the address already exists.
     */
    fun addContact(name: String, address: String): Boolean {
        if (!::prefs.isInitialized) {
            Log.e(TAG, "AddressBook not initialized — call init(context) first")
            return false
        }

        val trimmedName = name.trim()
        val trimmedAddress = address.trim()

        if (trimmedName.isBlank()) {
            Log.w(TAG, "Rejected contact: name is blank")
            return false
        }

        if (!isValidAddress(trimmedAddress)) {
            Log.w(TAG, "Rejected contact: invalid address format")
            return false
        }

        if (contacts.any { it.address == trimmedAddress }) {
            Log.w(TAG, "Rejected contact: address already exists in address book")
            return false
        }

        if (contacts.size >= MAX_CONTACTS) {
            Log.w(TAG, "Rejected contact: address book is full ($MAX_CONTACTS max)")
            return false
        }

        val contact = Contact(
            name = trimmedName,
            address = trimmedAddress,
            addedAtMillis = System.currentTimeMillis()
        )
        contacts.add(contact)
        saveToDisk()
        Log.d(TAG, "Added contact: \"$trimmedName\" -> ${trimmedAddress.take(10)}...")
        return true
    }

    /**
     * Remove a contact by wallet address.
     *
     * @param address The exact wallet address to remove.
     * @return true if a contact was found and removed, false otherwise.
     */
    fun removeContact(address: String): Boolean {
        if (!::prefs.isInitialized) {
            Log.e(TAG, "AddressBook not initialized — call init(context) first")
            return false
        }

        val trimmedAddress = address.trim()
        val removed = contacts.removeAll { it.address == trimmedAddress }
        if (removed) {
            saveToDisk()
            Log.d(TAG, "Removed contact with address ${trimmedAddress.take(10)}...")
        }
        return removed
    }

    /**
     * Get all contacts, ordered by most recently added first.
     *
     * @return Immutable list of all contacts. Empty list if none exist or
     *   the address book is not initialized.
     */
    fun getContacts(): List<Contact> {
        if (!::prefs.isInitialized) {
            Log.e(TAG, "AddressBook not initialized — call init(context) first")
            return emptyList()
        }
        return contacts.sortedByDescending { it.addedAtMillis }.toList()
    }

    /**
     * Look up a single contact by wallet address.
     *
     * @param address The wallet address to search for.
     * @return The matching Contact, or null if not found.
     */
    fun getContactByAddress(address: String): Contact? {
        if (!::prefs.isInitialized) {
            Log.e(TAG, "AddressBook not initialized — call init(context) first")
            return null
        }
        return contacts.find { it.address == address.trim() }
    }

    /**
     * Validate a Gratia wallet address format.
     *
     * Format: "grat:" prefix followed by 64 lowercase hex characters
     * representing the Ed25519 public key. Total length: 69.
     */
    private fun isValidAddress(address: String): Boolean {
        if (address.length != VALID_ADDRESS_LENGTH) return false
        if (!address.startsWith(ADDRESS_PREFIX)) return false

        // WHY: Check that the key portion is valid hex. This catches typos
        // and copy-paste errors before the user sends GRAT to a black hole.
        val keyPart = address.removePrefix(ADDRESS_PREFIX)
        return keyPart.all { it in '0'..'9' || it in 'a'..'f' }
    }

    // ── Persistence ────────────────────────────────────────────────────

    /**
     * Serialize all contacts to JSON and write to SharedPreferences.
     *
     * WHY: We write the entire array on every mutation rather than doing
     * incremental updates. With a 100-contact cap and ~100 bytes per entry,
     * the total payload is ~10 KB — negligible for SharedPreferences.
     */
    private fun saveToDisk() {
        try {
            val jsonArray = JSONArray()
            for (contact in contacts) {
                val obj = JSONObject().apply {
                    put("name", contact.name)
                    put("address", contact.address)
                    put("addedAtMillis", contact.addedAtMillis)
                }
                jsonArray.put(obj)
            }
            prefs.edit().putString(KEY_CONTACTS, jsonArray.toString()).apply()
        } catch (e: Exception) {
            Log.e(TAG, "Failed to save address book to disk", e)
        }
    }

    /**
     * Load contacts from SharedPreferences JSON. Silently drops any entries
     * that fail to parse — this handles data written by older app versions
     * or manual corruption without crashing.
     */
    private fun loadFromDisk() {
        contacts.clear()
        try {
            val raw = prefs.getString(KEY_CONTACTS, null) ?: return
            val jsonArray = JSONArray(raw)
            for (i in 0 until jsonArray.length()) {
                try {
                    val obj = jsonArray.getJSONObject(i)
                    val contact = Contact(
                        name = obj.getString("name"),
                        address = obj.getString("address"),
                        addedAtMillis = obj.getLong("addedAtMillis")
                    )
                    // Re-validate on load in case the format rules changed
                    // between app versions or the data was tampered with.
                    if (isValidAddress(contact.address) && contact.name.isNotBlank()) {
                        contacts.add(contact)
                    } else {
                        Log.w(TAG, "Skipped invalid contact on load at index $i")
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "Skipped malformed contact entry at index $i", e)
                }
            }
        } catch (e: Exception) {
            Log.e(TAG, "Failed to load address book from disk", e)
        }
    }
}
