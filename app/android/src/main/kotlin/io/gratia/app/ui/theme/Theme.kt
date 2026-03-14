package io.gratia.app.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

// ============================================================================
// Gratia Brand Colors
// ============================================================================

// WHY: Primary teal/cyan conveys trust and technology. The warm gold accent
// provides contrast and hints at the "earning" aspect of the app without
// looking like a typical crypto/finance app.

private val GratiaTeal = Color(0xFF00897B)          // Primary brand color
private val GratiaTealLight = Color(0xFF4DB6AC)      // Lighter variant
private val GratiaTealDark = Color(0xFF00695C)       // Darker variant
private val GratiaGold = Color(0xFFFFA726)           // Accent / reward highlights
private val GratiaGoldDark = Color(0xFFFF8F00)       // Accent in dark mode

// Surface and background tones
private val DarkSurface = Color(0xFF121212)
private val LightBackground = Color(0xFFFBFDF9)
private val LightSurface = Color(0xFFFFFFFF)

// Status colors
private val ErrorRed = Color(0xFFCF6679)
private val ErrorRedLight = Color(0xFFB00020)

// ============================================================================
// Color Schemes
// ============================================================================

private val DarkColorScheme = darkColorScheme(
    primary = GratiaTealLight,
    onPrimary = Color.Black,
    primaryContainer = GratiaTealDark,
    onPrimaryContainer = GratiaTealLight,
    secondary = GratiaGold,
    onSecondary = Color.Black,
    secondaryContainer = GratiaGoldDark,
    onSecondaryContainer = Color.White,
    tertiary = GratiaTealLight,
    background = DarkSurface,
    surface = DarkSurface,
    error = ErrorRed,
    onBackground = Color.White,
    onSurface = Color.White,
    onError = Color.Black,
)

private val LightColorScheme = lightColorScheme(
    primary = GratiaTeal,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFB2DFDB),
    onPrimaryContainer = GratiaTealDark,
    secondary = GratiaGoldDark,
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFFFE0B2),
    onSecondaryContainer = Color(0xFF5D4037),
    tertiary = GratiaTealDark,
    background = LightBackground,
    surface = LightSurface,
    error = ErrorRedLight,
    onBackground = Color(0xFF1C1B1F),
    onSurface = Color(0xFF1C1B1F),
    onError = Color.White,
)

// ============================================================================
// Theme Composable
// ============================================================================

/**
 * Gratia Material3 theme.
 *
 * Supports dynamic color on Android 12+ (Material You) with a fallback to
 * the Gratia brand color scheme on older devices.
 */
@Composable
fun GratiaTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    // WHY: Dynamic color (Material You) is enabled by default on Android 12+.
    // This makes the app feel native on Pixel and Samsung devices while still
    // using our brand colors on older phones (our target includes 2018+ devices
    // running Android 8.0 which don't support dynamic color).
    dynamicColor: Boolean = true,
    content: @Composable () -> Unit,
) {
    val colorScheme = when {
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        darkTheme -> DarkColorScheme
        else -> LightColorScheme
    }

    MaterialTheme(
        colorScheme = colorScheme,
        content = content,
    )
}
