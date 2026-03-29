package io.gratia.app.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import io.gratia.app.GratiaLogo
import io.gratia.app.ui.theme.AmberGold
import io.gratia.app.ui.theme.DeepNavy
import io.gratia.app.ui.theme.WarmWhite
import kotlinx.coroutines.delay

// WHY: 2-second delay gives the Rust core and sensor managers time to initialize
// while presenting a polished brand impression on launch.
private const val SPLASH_DISPLAY_MILLIS = 2000L

// WHY: Subtle 5% scale pulse keeps the splash feeling alive without being distracting.
// 1.5s round-trip is slow enough to read as a calm "breathing" effect.
private const val PULSE_DURATION_MILLIS = 1500
private const val PULSE_MIN_SCALE = 1.0f
private const val PULSE_MAX_SCALE = 1.05f

@Composable
fun SplashScreen(onTimeout: () -> Unit) {
    LaunchedEffect(Unit) {
        delay(SPLASH_DISPLAY_MILLIS)
        onTimeout()
    }

    val infiniteTransition = rememberInfiniteTransition(label = "logoPulse")
    val scale by infiniteTransition.animateFloat(
        initialValue = PULSE_MIN_SCALE,
        targetValue = PULSE_MAX_SCALE,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = PULSE_DURATION_MILLIS),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "logoScale",
    )

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(DeepNavy),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            GratiaLogo(
                modifier = Modifier.scale(scale),
                size = 120,
            )

            Spacer(modifier = Modifier.height(16.dp))

            Text(
                text = "Gratia",
                style = MaterialTheme.typography.headlineLarge,
                fontWeight = FontWeight.Bold,
                color = WarmWhite,
            )

            Spacer(modifier = Modifier.height(8.dp))

            Text(
                text = "Decentralized. Human. Fair.",
                style = MaterialTheme.typography.bodyMedium,
                color = WarmWhite.copy(alpha = 0.6f),
            )
        }

        CircularProgressIndicator(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(bottom = 64.dp)
                .size(32.dp),
            color = AmberGold,
            strokeWidth = 2.dp,
        )
    }
}
