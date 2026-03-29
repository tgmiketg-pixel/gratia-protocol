package io.gratia.app.ui

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.view.HapticFeedbackConstants
import android.view.View
import androidx.compose.runtime.Composable
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalView

/**
 * Haptic feedback utility for the Gratia app.
 *
 * WHY: Tactile feedback makes the app feel responsive and premium.
 * Users get physical confirmation that actions succeeded (transaction sent,
 * mining started, authentication passed). This is especially important
 * for financial operations where visual feedback alone can feel uncertain.
 */
object GratiaHaptics {

    /** Light tap — button presses, navigation. */
    fun tick(view: View) {
        view.performHapticFeedback(HapticFeedbackConstants.CLOCK_TICK)
    }

    /** Medium click — successful auth, mining state change. */
    fun click(view: View) {
        view.performHapticFeedback(HapticFeedbackConstants.CONFIRM)
    }

    /** Strong confirmation — transaction sent, pattern/PIN accepted. */
    fun confirm(context: Context) {
        val vibrator = getVibrator(context) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            // WHY: Double-pulse pattern feels like a "success" confirmation.
            // 50ms pulse, 80ms gap, 50ms pulse.
            vibrator.vibrate(
                VibrationEffect.createWaveform(
                    longArrayOf(0, 50, 80, 50),
                    -1, // no repeat
                )
            )
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(100)
        }
    }

    /** Error buzz — wrong PIN, failed auth. */
    fun error(context: Context) {
        val vibrator = getVibrator(context) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            // WHY: Three short rapid pulses feels like a rejection/error.
            vibrator.vibrate(
                VibrationEffect.createWaveform(
                    longArrayOf(0, 30, 50, 30, 50, 30),
                    -1,
                )
            )
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(150)
        }
    }

    /** Mining started — gentle ascending pulse. */
    fun miningStarted(context: Context) {
        val vibrator = getVibrator(context) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            vibrator.vibrate(
                VibrationEffect.createWaveform(
                    longArrayOf(0, 20, 40, 40, 40, 60),
                    intArrayOf(0, 50, 0, 100, 0, 200),
                    -1,
                )
            )
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(200)
        }
    }

    private fun getVibrator(context: Context): Vibrator? {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val manager = context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager
            manager?.defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            context.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
        }
    }
}
