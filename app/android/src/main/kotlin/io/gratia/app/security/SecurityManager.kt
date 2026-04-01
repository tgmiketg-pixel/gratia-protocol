package io.gratia.app.security

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.security.MessageDigest

/**
 * Manages app security: lock method selection, credential storage, and auth state.
 *
 * WHY: Gratia holds real value (GRAT tokens). If someone picks up an unlocked
 * phone, they should not be able to open the wallet or send funds without
 * authenticating. This mirrors standard wallet security (Coinbase, Trust Wallet,
 * MetaMask) but adds pattern lock as an option for users in regions where
 * fingerprint readers are uncommon on budget phones.
 */
object SecurityManager {

    private const val TAG = "GratiaSecurityManager"

    private const val PREFS_FILE = "gratia_security_prefs"
    private const val KEY_LOCK_METHOD = "lock_method"
    private const val KEY_PIN_HASH = "pin_hash"
    private const val KEY_PATTERN_HASH = "pattern_hash"
    private const val KEY_APP_LOCK_ENABLED = "app_lock_enabled"
    private const val KEY_TX_AUTH_ENABLED = "tx_auth_enabled"

    /**
     * Grace period in milliseconds. If the user authenticated within this window,
     * skip the lock screen when returning to the app.
     *
     * WHY: 60 seconds prevents annoying re-prompts when the user briefly switches
     * apps (e.g., to check a message) and comes back. Long enough to be convenient,
     * short enough that a stolen phone can't be used after a minute.
     */
    private const val GRACE_PERIOD_MS = 60_000L

    private var encryptedPrefs: SharedPreferences? = null
    private var lastAuthTimeMs: Long = 0L

    /** Available lock methods. */
    enum class LockMethod {
        NONE,
        BIOMETRIC,
        PIN,
        PATTERN,
        DEVICE_CREDENTIAL,
    }

    fun init(context: Context) {
        if (encryptedPrefs != null) return
        try {
            val masterKey = MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()

            encryptedPrefs = EncryptedSharedPreferences.create(
                context,
                PREFS_FILE,
                masterKey,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
            Log.i(TAG, "SecurityManager initialized with encrypted storage")
        } catch (e: Exception) {
            // WHY: On some budget devices, Keystore initialization fails.
            // Fall back to standard SharedPreferences so the app still works.
            Log.e(TAG, "EncryptedSharedPreferences failed, falling back: ${e.message}")
            encryptedPrefs = context.getSharedPreferences(PREFS_FILE, Context.MODE_PRIVATE)
        }
    }

    private fun prefs(): SharedPreferences {
        return encryptedPrefs ?: throw IllegalStateException("SecurityManager not initialized")
    }

    // ========================================================================
    // Lock method configuration
    // ========================================================================

    var appLockEnabled: Boolean
        get() = prefs().getBoolean(KEY_APP_LOCK_ENABLED, false)
        set(value) = prefs().edit().putBoolean(KEY_APP_LOCK_ENABLED, value).apply()

    /** Whether to require auth before sending tokens. Defaults to true once any lock is set. */
    var txAuthEnabled: Boolean
        get() = prefs().getBoolean(KEY_TX_AUTH_ENABLED, appLockEnabled)
        set(value) = prefs().edit().putBoolean(KEY_TX_AUTH_ENABLED, value).apply()

    var lockMethod: LockMethod
        get() {
            val name = prefs().getString(KEY_LOCK_METHOD, LockMethod.NONE.name)
            return try {
                LockMethod.valueOf(name ?: LockMethod.NONE.name)
            } catch (_: Exception) {
                LockMethod.NONE
            }
        }
        set(value) = prefs().edit().putString(KEY_LOCK_METHOD, value.name).apply()

    // ========================================================================
    // Credential management
    // ========================================================================

    /** Store a 5-digit PIN as a SHA-256 hash. */
    fun setPin(pin: String) {
        require(pin.length == 5 && pin.all { it.isDigit() }) { "PIN must be exactly 5 digits" }
        prefs().edit().putString(KEY_PIN_HASH, sha256(pin)).apply()
        lockMethod = LockMethod.PIN
        appLockEnabled = true
        txAuthEnabled = true
        Log.i(TAG, "PIN set")
    }

    /** Verify a PIN against the stored hash. */
    fun verifyPin(pin: String): Boolean {
        val stored = prefs().getString(KEY_PIN_HASH, null) ?: return false
        return sha256(pin) == stored
    }

    /**
     * Store a pattern as a SHA-256 hash.
     * Pattern is encoded as a comma-separated list of dot indices (0-8).
     */
    fun setPattern(pattern: List<Int>) {
        require(pattern.size >= 4) { "Pattern must connect at least 4 dots" }
        val encoded = pattern.joinToString(",")
        prefs().edit().putString(KEY_PATTERN_HASH, sha256(encoded)).apply()
        lockMethod = LockMethod.PATTERN
        appLockEnabled = true
        txAuthEnabled = true
        Log.i(TAG, "Pattern set")
    }

    /** Verify a pattern against the stored hash. */
    fun verifyPattern(pattern: List<Int>): Boolean {
        val stored = prefs().getString(KEY_PATTERN_HASH, null) ?: return false
        val encoded = pattern.joinToString(",")
        return sha256(encoded) == stored
    }

    /** Enable biometric lock (no credential to store — hardware handles it). */
    fun enableBiometric() {
        lockMethod = LockMethod.BIOMETRIC
        appLockEnabled = true
        txAuthEnabled = true
        Log.i(TAG, "Biometric lock enabled")
    }

    /** Enable device credential lock (delegates to system lock screen). */
    fun enableDeviceCredential() {
        lockMethod = LockMethod.DEVICE_CREDENTIAL
        appLockEnabled = true
        txAuthEnabled = true
        Log.i(TAG, "Device credential lock enabled")
    }

    /** Disable all locks. */
    fun disableLock() {
        lockMethod = LockMethod.NONE
        appLockEnabled = false
        txAuthEnabled = false
        prefs().edit()
            .remove(KEY_PIN_HASH)
            .remove(KEY_PATTERN_HASH)
            .apply()
        Log.i(TAG, "All locks disabled")
    }

    /** Check if a PIN has been stored. */
    fun hasPinSet(): Boolean = prefs().getString(KEY_PIN_HASH, null) != null

    /** Check if a pattern has been stored. */
    fun hasPatternSet(): Boolean = prefs().getString(KEY_PATTERN_HASH, null) != null

    // ========================================================================
    // Auth state and grace period
    // ========================================================================

    /** Call after successful authentication. Resets the grace timer. */
    fun onAuthSuccess() {
        // WHY: Use elapsedRealtime (monotonic clock) instead of currentTimeMillis
        // to prevent bypass by changing device clock.
        lastAuthTimeMs = android.os.SystemClock.elapsedRealtime()
    }

    /** True if the user authenticated recently enough to skip the lock screen. */
    fun isWithinGracePeriod(): Boolean {
        if (lastAuthTimeMs == 0L) return false
        return (android.os.SystemClock.elapsedRealtime() - lastAuthTimeMs) < GRACE_PERIOD_MS
    }

    /** Whether the lock screen should be shown right now. */
    fun shouldShowLockScreen(): Boolean {
        if (!appLockEnabled) return false
        if (lockMethod == LockMethod.NONE) return false
        if (isWithinGracePeriod()) return false
        return true
    }

    /** Whether transaction auth is needed right now. */
    fun shouldAuthForTransaction(): Boolean {
        if (!txAuthEnabled) return false
        if (lockMethod == LockMethod.NONE) return false
        // WHY: Don't skip tx auth during grace period — every send should confirm.
        // The grace period only applies to app unlock, not fund transfers.
        return true
    }

    // ========================================================================
    // Utility
    // ========================================================================

    private fun sha256(input: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
        val hash = digest.digest(input.toByteArray(Charsets.UTF_8))
        return hash.joinToString("") { "%02x".format(it) }
    }
}
