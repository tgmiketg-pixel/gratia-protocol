package io.gratia.app.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.BatteryChargingFull
import androidx.compose.material.icons.filled.BatteryFull
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Power
import androidx.compose.material.icons.filled.PowerOff
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

// ============================================================================
// MiningScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MiningScreen(
    viewModel: MiningViewModel = viewModel(),
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Mining") },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surface,
                ),
            )
        },
    ) { padding ->
        if (state.isLoading) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
                contentAlignment = Alignment.Center,
            ) {
                CircularProgressIndicator()
            }
        } else {
            MiningContent(
                state = state,
                onStartMining = { viewModel.startMining() },
                onStopMining = { viewModel.stopMining() },
                modifier = Modifier.padding(padding),
            )
        }
    }
}

@Composable
private fun MiningContent(
    state: MiningUiState,
    onStartMining: () -> Unit,
    onStopMining: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val mining = state.miningStatus ?: return
    val pol = state.polStatus

    LazyColumn(
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        modifier = modifier.fillMaxSize(),
    ) {
        // Mining state indicator
        item {
            MiningStateCard(mining, onStartMining, onStopMining)
        }

        // Battery and power status
        item {
            BatteryStatusCard(mining)
        }

        // Proof of Life status
        if (pol != null) {
            item {
                ProofOfLifeCard(pol)
            }
        }

        // Presence Score (only show if above threshold)
        if (mining.presenceScore > 0) {
            item {
                PresenceScoreCard(mining.presenceScore)
            }
        }

        // Earnings summary
        item {
            EarningsCard(
                earningsToday = state.earningsToday,
                earningsThisWeek = state.earningsThisWeek,
                earningsTotal = state.earningsTotal,
            )
        }
    }
}

// ============================================================================
// Mining State Card
// ============================================================================

@Composable
private fun MiningStateCard(
    status: MiningStatus,
    onStartMining: () -> Unit,
    onStopMining: () -> Unit,
) {
    val stateColor = when (status.state) {
        "mining" -> Color(0xFF4CAF50)          // Green
        "proof_of_life" -> Color(0xFF2196F3)   // Blue
        "battery_low" -> Color(0xFFFFC107)     // Yellow
        "throttled" -> Color(0xFFFF9800)       // Orange
        "pending_activation" -> Color(0xFF9E9E9E) // Gray
        else -> MaterialTheme.colorScheme.outline
    }

    val isMining = status.state == "mining"

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = stateColor.copy(alpha = 0.12f),
        ),
    ) {
        Column(
            modifier = Modifier.padding(20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // Animated indicator when mining
            Box(
                modifier = Modifier.size(80.dp),
                contentAlignment = Alignment.Center,
            ) {
                if (isMining) {
                    MiningPulseAnimation(stateColor)
                }
                Canvas(modifier = Modifier.size(48.dp)) {
                    drawCircle(color = stateColor)
                }
            }

            Spacer(modifier = Modifier.height(12.dp))

            Text(
                text = miningStateLabel(status.state),
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
                color = stateColor,
            )

            Text(
                text = miningStateDescription(status),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
            )

            Spacer(modifier = Modifier.height(16.dp))

            // Start / Stop button
            if (isMining) {
                OutlinedButton(
                    onClick = onStopMining,
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) {
                    Icon(
                        Icons.Default.Stop,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Stop Mining")
                }
            } else if (status.state == "proof_of_life" && status.isPluggedIn && status.batteryPercent >= 80 && status.currentDayPolValid) {
                Button(onClick = onStartMining) {
                    Icon(
                        Icons.Default.PlayArrow,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Start Mining")
                }
            }
        }
    }
}

@Composable
private fun MiningPulseAnimation(color: Color) {
    val infiniteTransition = rememberInfiniteTransition(label = "mining_pulse")
    val alpha by infiniteTransition.animateFloat(
        initialValue = 0.3f,
        targetValue = 0.0f,
        animationSpec = infiniteRepeatable(
            animation = tween(
                durationMillis = 1500, // Smooth 1.5s pulse cycle
                easing = LinearEasing,
            ),
            repeatMode = RepeatMode.Restart,
        ),
        label = "pulse_alpha",
    )
    val scale by infiniteTransition.animateFloat(
        initialValue = 1.0f,
        targetValue = 1.6f,
        animationSpec = infiniteRepeatable(
            animation = tween(
                durationMillis = 1500,
                easing = LinearEasing,
            ),
            repeatMode = RepeatMode.Restart,
        ),
        label = "pulse_scale",
    )

    Canvas(
        modifier = Modifier
            .size(80.dp)
            .alpha(alpha),
    ) {
        drawCircle(
            color = color,
            radius = size.minDimension / 2 * scale,
        )
    }
}

private fun miningStateDescription(status: MiningStatus): String = when (status.state) {
    "mining" -> "Earning GRAT at flat rate"
    "proof_of_life" -> "Passively collecting sensor data"
    "battery_low" -> "Battery at ${status.batteryPercent}% — need 80%+"
    "throttled" -> "CPU temperature too high — workload reduced"
    "pending_activation" -> "Waiting for mining conditions to be met"
    else -> ""
}

// ============================================================================
// Battery Status Card
// ============================================================================

@Composable
private fun BatteryStatusCard(status: MiningStatus) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = "Power Status",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )

            Spacer(modifier = Modifier.height(12.dp))

            // Battery progress
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = if (status.isPluggedIn) {
                        Icons.Default.BatteryChargingFull
                    } else {
                        Icons.Default.BatteryFull
                    },
                    contentDescription = null,
                    tint = when {
                        status.batteryPercent >= 80 -> Color(0xFF4CAF50)
                        status.batteryPercent >= 50 -> Color(0xFFFFC107)
                        else -> MaterialTheme.colorScheme.error
                    },
                    modifier = Modifier.size(24.dp),
                )
                Spacer(modifier = Modifier.width(12.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(
                            text = "Battery",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Text(
                            text = "${status.batteryPercent}%",
                            style = MaterialTheme.typography.bodyMedium,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                    Spacer(modifier = Modifier.height(4.dp))
                    LinearProgressIndicator(
                        progress = { status.batteryPercent / 100f },
                        modifier = Modifier.fillMaxWidth(),
                        trackColor = MaterialTheme.colorScheme.surfaceVariant,
                    )
                }
            }

            Spacer(modifier = Modifier.height(12.dp))

            // Plugged in status
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = if (status.isPluggedIn) Icons.Default.Power else Icons.Default.PowerOff,
                    contentDescription = null,
                    tint = if (status.isPluggedIn) Color(0xFF4CAF50) else MaterialTheme.colorScheme.outline,
                    modifier = Modifier.size(24.dp),
                )
                Spacer(modifier = Modifier.width(12.dp))
                Text(
                    text = if (status.isPluggedIn) "Connected to power" else "Not connected to power",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
    }
}

// ============================================================================
// Proof of Life Card
// ============================================================================

/** All PoL parameter keys in display order, matching the FFI parameter names. */
private val allPolParameters = listOf(
    "unlocks" to "10+ unlock events",
    "unlock_spread" to "Unlocks spread across 6+ hours",
    "interactions" to "Screen interaction sessions",
    "orientation" to "Orientation change detected",
    "motion" to "Human-consistent motion",
    "gps" to "GPS fix obtained",
    "network" to "Wi-Fi or Bluetooth connectivity",
    "bt_variation" to "Bluetooth environment variation",
    "charge_event" to "Charge cycle event",
)

@Composable
private fun ProofOfLifeCard(polStatus: ProofOfLifeStatus) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Proof of Life",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                val validColor = if (polStatus.isValidToday) Color(0xFF4CAF50) else Color(0xFFFFA000)
                Text(
                    text = if (polStatus.isValidToday) "Valid" else "Incomplete",
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = validColor,
                )
            }

            if (polStatus.consecutiveDays > 0) {
                Text(
                    text = "${polStatus.consecutiveDays} consecutive days",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                )
            }

            Spacer(modifier = Modifier.height(12.dp))

            // Parameter checklist
            allPolParameters.forEach { (key, label) ->
                val met = polStatus.parametersMet.contains(key)
                PolParameterRow(label = label, isMet = met)
            }
        }
    }
}

@Composable
private fun PolParameterRow(label: String, isMet: Boolean) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            imageVector = if (isMet) Icons.Default.Check else Icons.Default.Close,
            contentDescription = if (isMet) "Met" else "Not met",
            tint = if (isMet) Color(0xFF4CAF50) else MaterialTheme.colorScheme.outline.copy(alpha = 0.5f),
            modifier = Modifier.size(18.dp),
        )
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.bodySmall,
            color = if (isMet) {
                MaterialTheme.colorScheme.onSurface
            } else {
                MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f)
            },
        )
    }
}

// ============================================================================
// Presence Score Card
// ============================================================================

@Composable
private fun PresenceScoreCard(score: Int) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = "Presence Score",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(modifier = Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.Bottom) {
                Text(
                    text = "$score",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    text = " / 100",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    modifier = Modifier.padding(bottom = 4.dp),
                )
            }
            Spacer(modifier = Modifier.height(4.dp))
            LinearProgressIndicator(
                progress = { score / 100f },
                modifier = Modifier.fillMaxWidth(),
                trackColor = MaterialTheme.colorScheme.surfaceVariant,
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "Affects block production selection probability only. Does not affect mining rewards.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
            )
        }
    }
}

// ============================================================================
// Earnings Card
// ============================================================================

@Composable
private fun EarningsCard(
    earningsToday: Long,
    earningsThisWeek: Long,
    earningsTotal: Long,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = "Mining Earnings",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )

            Spacer(modifier = Modifier.height(12.dp))

            EarningsRow("Today", earningsToday)
            EarningsRow("This Week", earningsThisWeek)
            EarningsRow("Total", earningsTotal)
        }
    }
}

@Composable
private fun EarningsRow(label: String, amountLux: Long) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
        )
        Text(
            text = "${formatGrat(amountLux)} GRAT",
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
        )
    }
}
