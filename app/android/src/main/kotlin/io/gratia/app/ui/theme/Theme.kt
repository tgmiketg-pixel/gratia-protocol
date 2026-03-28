package io.gratia.app.ui.theme

import android.app.Activity
import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

// ============================================================================
// Gratia Brand Colors — derived from Brand Identity Guide v1.0
// Every color traces back to the logo SVG. No off-brand colors allowed.
// ============================================================================

// --- Primary ---
val DeepNavy = Color(0xFF1A2744)           // Primary dark — trust, confidence
val AmberGold = Color(0xFFF5A623)          // Primary accent — value, warmth

// --- Secondary: The Instrument Palette ---
val DarkGoldenrod = Color(0xFFB8860B)      // Borders, rules
val DarkAmber = Color(0xFFD4890F)          // Secondary accent
val Golden = Color(0xFFE8A020)             // Highlights
val AgedGold = Color(0xFF8B6914)           // Muted details

// --- Extended Palette ---
val Midnight = Color(0xFF0D1527)           // Deep background (dark mode)
val CharcoalNavy = Color(0xFF2A3A5C)       // Body text on light, secondary on dark
val WarmWhite = Color(0xFFFAF5EB)          // Light background
val OffWhite = Color(0xFFF0E8D8)           // Card background (light mode)
val LightGold = Color(0xFFFDD888)          // Soft accent
val PaleAmber = Color(0xFFFEF3D5)          // Tint background

// --- Status ---
val SignalGreen = Color(0xFF2ECC71)        // Active / success
val AlertRed = Color(0xFFE74C3C)           // Error / warning

// ============================================================================
// Color Schemes
// ============================================================================

private val DarkColorScheme = darkColorScheme(
    primary = AmberGold,
    onPrimary = DeepNavy,
    primaryContainer = CharcoalNavy,
    onPrimaryContainer = LightGold,
    secondary = Golden,
    onSecondary = Midnight,
    secondaryContainer = DarkGoldenrod,
    onSecondaryContainer = PaleAmber,
    tertiary = LightGold,
    onTertiary = DeepNavy,
    background = Midnight,
    surface = DeepNavy,
    surfaceVariant = CharcoalNavy,
    onBackground = WarmWhite,
    onSurface = WarmWhite,
    onSurfaceVariant = OffWhite,
    error = AlertRed,
    onError = Color.White,
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
    outline = AgedGold,
    outlineVariant = CharcoalNavy,
)

private val LightColorScheme = lightColorScheme(
    primary = DeepNavy,
    onPrimary = WarmWhite,
    primaryContainer = PaleAmber,
    onPrimaryContainer = DeepNavy,
    secondary = AmberGold,
    onSecondary = DeepNavy,
    secondaryContainer = LightGold,
    onSecondaryContainer = DeepNavy,
    tertiary = DarkGoldenrod,
    onTertiary = Color.White,
    background = WarmWhite,
    surface = Color.White,
    surfaceVariant = OffWhite,
    onBackground = DeepNavy,
    onSurface = DeepNavy,
    onSurfaceVariant = CharcoalNavy,
    error = AlertRed,
    onError = Color.White,
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF93000A),
    outline = AgedGold,
    outlineVariant = OffWhite,
)

// ============================================================================
// Theme Composable
// ============================================================================

/**
 * Gratia Material3 theme.
 *
 * WHY: Dynamic color (Material You) is disabled by default. The Gratia brand
 * identity guide mandates consistent navy/gold across all devices. The brand
 * must be recognizable regardless of the user's wallpaper or system theme.
 */
@Composable
fun GratiaTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    val colorScheme = if (darkTheme) DarkColorScheme else LightColorScheme

    // WHY: Tint the system bars to match the brand. Status bar uses the
    // deepest navy so the app feels immersive from the top edge.
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            window.statusBarColor = if (darkTheme) {
                Midnight.toArgb()
            } else {
                DeepNavy.toArgb()
            }
            WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = false
        }
    }

    MaterialTheme(
        colorScheme = colorScheme,
        content = content,
    )
}
