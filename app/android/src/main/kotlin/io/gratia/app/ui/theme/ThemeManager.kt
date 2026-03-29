package io.gratia.app.ui.theme

import android.content.Context
import android.content.SharedPreferences

/**
 * Manages the user's theme preference: dark, light, or follow system.
 *
 * WHY: Some users prefer a specific theme regardless of system settings.
 * The default "follow system" respects Android's global dark mode toggle,
 * but users can override it here.
 */
object ThemeManager {

    private const val PREFS_NAME = "gratia_theme_prefs"
    private const val KEY_THEME_MODE = "theme_mode"

    enum class ThemeMode {
        SYSTEM,
        DARK,
        LIGHT,
    }

    private var prefs: SharedPreferences? = null

    fun init(context: Context) {
        if (prefs != null) return
        prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
    }

    var themeMode: ThemeMode
        get() {
            val name = prefs?.getString(KEY_THEME_MODE, ThemeMode.SYSTEM.name)
            return try {
                ThemeMode.valueOf(name ?: ThemeMode.SYSTEM.name)
            } catch (_: Exception) {
                ThemeMode.SYSTEM
            }
        }
        set(value) {
            prefs?.edit()?.putString(KEY_THEME_MODE, value.name)?.apply()
        }
}
