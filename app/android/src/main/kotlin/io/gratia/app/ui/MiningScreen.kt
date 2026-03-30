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
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.BatteryChargingFull
import androidx.compose.material.icons.filled.BatteryFull
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Power
import androidx.compose.material.icons.filled.PowerOff
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material3.AlertDialog
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
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import io.gratia.app.GratiaLogo
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import io.gratia.app.ui.theme.*

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
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                GratiaLogo(size = 56)
                Spacer(modifier = Modifier.width(12.dp))
                Text(
                    "Mining",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = androidx.compose.ui.text.font.FontWeight.Bold,
                )
            }
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
                onStakeClick = { viewModel.showStakeDialog() },
                onUnstakeClick = { viewModel.showUnstakeDialog() },
                modifier = Modifier.padding(padding),
            )
        }

        // Stake dialog
        if (state.showStakeDialog) {
            StakeAmountDialog(
                title = "Stake GRAT",
                confirmLabel = "Stake",
                errorMessage = state.stakeError,
                onConfirm = { amountGrat ->
                    // WHY: Convert whole GRAT to Lux (1 GRAT = 1,000,000 Lux)
                    // because the FFI bridge operates in the smallest unit.
                    val amountLux = amountGrat * 1_000_000L
                    viewModel.stake(amountLux)
                },
                onDismiss = { viewModel.hideStakeDialog() },
            )
        }

        // Unstake dialog
        if (state.showUnstakeDialog) {
            StakeAmountDialog(
                title = "Unstake GRAT",
                confirmLabel = "Unstake",
                errorMessage = state.stakeError,
                onConfirm = { amountGrat ->
                    val amountLux = amountGrat * 1_000_000L
                    viewModel.unstake(amountLux)
                },
                onDismiss = { viewModel.hideUnstakeDialog() },
            )
        }
    }
}

@Composable
private fun MiningContent(
    state: MiningUiState,
    onStartMining: () -> Unit,
    onStopMining: () -> Unit,
    onStakeClick: () -> Unit,
    onUnstakeClick: () -> Unit,
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
            MiningStateCard(
                status = mining,
                onStartMining = onStartMining,
                onStopMining = onStopMining,
                blockHeight = state.blockHeight,
                currentSlot = state.currentSlot,
            )
        }

        // Consensus & network status
        item {
            ConsensusInfoCard(
                peerCount = state.peerCount,
                blockHeight = state.blockHeight,
                currentSlot = state.currentSlot,
                isCommitteeMember = state.isCommitteeMember,
                blocksProduced = state.blocksProduced,
                syncStatus = state.syncStatus,
                isNetworkRunning = state.isNetworkRunning,
            )
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

        // Presence Score — always visible so users know the metric exists
        item {
            PresenceScoreCard(mining.presenceScore)
        }

        // Earnings summary
        item {
            EarningsCard(
                earningsToday = state.earningsToday,
                earningsThisWeek = state.earningsThisWeek,
                earningsTotal = state.earningsTotal,
            )
        }

        // Staking
        item {
            StakingCard(
                stakeInfo = state.stakeInfo,
                onStakeClick = onStakeClick,
                onUnstakeClick = onUnstakeClick,
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
    blockHeight: Long = 0L,
    currentSlot: Long = 0L,
) {
    // WHY: Mining animation should only show when consensus is actively
    // running AND power conditions are met. Without the consensus check,
    // the animation runs indefinitely after consensus stops — misleading
    // the user into thinking they're earning when no blocks are produced.
    val isMining = status.isConsensusActive && (
        status.state == "mining" ||
        status.earnedThisSessionLux > 0 ||
        (status.isPluggedIn && status.batteryPercent >= 80)
    )

    val stateColor = if (isMining) {
        SignalGreen
    } else {
        when (status.state) {
            "mining" -> SignalGreen
            "proof_of_life" -> CharcoalNavy
            "battery_low" -> AmberGold
            "throttled" -> DarkAmber
            "pending_activation" -> AgedGold
            else -> MaterialTheme.colorScheme.outline
        }
    }

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
            if (isMining) {
                // ── Block Pulse Ring Animation ─────────────────────────
                // WHY: Shows real consensus data instead of a generic spinner.
                // The ring fills over 4 seconds (one slot), then pulses when
                // a block is finalized. Green pulse = BFT finality (peer
                // co-signed). The user can see the blockchain's heartbeat.

                // Track block height changes to trigger finality pulse
                var lastFinalizedHeight by remember { mutableLongStateOf(0L) }
                var pulseTriggered by remember { mutableStateOf(false) }
                var pulseStartTime by remember { mutableLongStateOf(0L) }

                // WHY: When blockHeight increases, a new block was finalized.
                // Trigger a radial pulse outward to visually confirm consensus.
                LaunchedEffect(blockHeight) {
                    if (blockHeight > lastFinalizedHeight && lastFinalizedHeight > 0L) {
                        pulseTriggered = true
                        pulseStartTime = System.currentTimeMillis()
                    }
                    lastFinalizedHeight = blockHeight
                }

                // Slot progress: fills from 0 to 1 over 4 seconds, then resets
                val slotProgress by animateFloatAsState(
                    targetValue = if (currentSlot % 2 == 0L) 1f else 0f,
                    animationSpec = tween(
                        durationMillis = 3800,
                        easing = LinearEasing,
                    ),
                    label = "slot_fill",
                )

                // Continuous slot fill using infinite transition
                val infiniteTransition = rememberInfiniteTransition(label = "slot_ring")
                val slotFill by infiniteTransition.animateFloat(
                    initialValue = 0f,
                    targetValue = 1f,
                    animationSpec = infiniteRepeatable(
                        animation = tween(4000, easing = LinearEasing),
                        repeatMode = RepeatMode.Restart,
                    ),
                    label = "slot_fill_continuous",
                )

                // Finality pulse: expands outward when a block is finalized
                val pulseScale by animateFloatAsState(
                    targetValue = if (pulseTriggered) 1.6f else 1.0f,
                    animationSpec = tween(
                        durationMillis = 600,
                        easing = androidx.compose.animation.core.EaseOut,
                    ),
                    finishedListener = { pulseTriggered = false },
                    label = "finality_pulse_scale",
                )
                val pulseAlpha by animateFloatAsState(
                    targetValue = if (pulseTriggered) 0f else 0.6f,
                    animationSpec = tween(
                        durationMillis = 600,
                        easing = androidx.compose.animation.core.EaseOut,
                    ),
                    label = "finality_pulse_alpha",
                )

                // Inner glow pulses gently
                val glow by infiniteTransition.animateFloat(
                    initialValue = 0.4f,
                    targetValue = 0.8f,
                    animationSpec = infiniteRepeatable(
                        animation = tween(2000, easing = LinearEasing),
                        repeatMode = RepeatMode.Reverse,
                    ),
                    label = "inner_glow",
                )

                Box(
                    modifier = Modifier.size(140.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    // Layer 1: Finality pulse ring (expands on block finalization)
                    Canvas(
                        modifier = Modifier
                            .size(130.dp)
                            .graphicsLayer {
                                scaleX = pulseScale
                                scaleY = pulseScale
                                alpha = pulseAlpha
                            },
                    ) {
                        drawCircle(
                            color = stateColor,
                            radius = size.minDimension / 2,
                            style = androidx.compose.ui.graphics.drawscope.Stroke(width = 4.dp.toPx()),
                        )
                    }

                    // Layer 2: Slot progress ring (fills over 4 seconds)
                    Canvas(modifier = Modifier.size(110.dp)) {
                        // Background track
                        drawArc(
                            color = stateColor.copy(alpha = 0.15f),
                            startAngle = -90f,
                            sweepAngle = 360f,
                            useCenter = false,
                            style = androidx.compose.ui.graphics.drawscope.Stroke(
                                width = 6.dp.toPx(),
                                cap = androidx.compose.ui.graphics.StrokeCap.Round,
                            ),
                        )
                        // Fill arc — sweeps 360 degrees over one slot (4 seconds)
                        drawArc(
                            color = stateColor,
                            startAngle = -90f,
                            sweepAngle = slotFill * 360f,
                            useCenter = false,
                            style = androidx.compose.ui.graphics.drawscope.Stroke(
                                width = 6.dp.toPx(),
                                cap = androidx.compose.ui.graphics.StrokeCap.Round,
                            ),
                        )
                    }

                    // Layer 3: Inner glow
                    Canvas(modifier = Modifier.size(70.dp)) {
                        drawCircle(color = stateColor.copy(alpha = glow * 0.25f))
                    }

                    // Layer 4: Block height number in center
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(
                            text = "$blockHeight",
                            style = MaterialTheme.typography.headlineMedium,
                            fontWeight = FontWeight.Bold,
                            color = stateColor,
                        )
                        Text(
                            text = "blocks",
                            style = MaterialTheme.typography.labelSmall,
                            color = stateColor.copy(alpha = 0.7f),
                        )
                    }
                }

                Spacer(modifier = Modifier.height(12.dp))

                Text(
                    text = "Mining",
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                    color = stateColor,
                )
                Text(
                    text = "Producing blocks and earning GRAT",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                    textAlign = TextAlign.Center,
                )

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "${formatGrat(status.earnedThisSessionLux)} GRAT",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.Bold,
                    color = stateColor,
                )
                Text(
                    text = "earned so far",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                )
            } else {
                // Not mining — show static state with clear reason
                val notMiningLabel: String
                val notMiningDescription: String

                if (!status.isConsensusActive) {
                    notMiningLabel = "Not Mining"
                    notMiningDescription = "Consensus is not running — no blocks being produced"
                } else {
                    notMiningLabel = miningStateLabel(status.state)
                    notMiningDescription = miningStateDescription(status)
                }

                Box(
                    modifier = Modifier.size(80.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Canvas(modifier = Modifier.size(56.dp)) {
                        drawCircle(color = stateColor)
                    }
                    // WHY: Show a clear "off" icon so the user instantly sees
                    // mining is inactive, rather than just a colored circle.
                    Icon(
                        Icons.Default.PowerOff,
                        contentDescription = null,
                        tint = Color.White,
                        modifier = Modifier.size(28.dp),
                    )
                }

                Spacer(modifier = Modifier.height(12.dp))

                Text(
                    text = notMiningLabel,
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                    color = stateColor,
                )
                Text(
                    text = notMiningDescription,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                    textAlign = TextAlign.Center,
                )
            }

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
            } else if ((status.state == "proof_of_life" || status.state == "pending_activation") && status.isPluggedIn && status.batteryPercent >= 80 && status.currentDayPolValid) {
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

/**
 * Pulsing ring animation behind the mining state circle.
 *
 * WHY: Uses graphicsLayer instead of Modifier.alpha() for the fade effect.
 * Modifier.alpha() can fail to animate on some Samsung/MediaTek devices because
 * it triggers a full recomposition path. graphicsLayer operates at the render
 * layer (hardware-accelerated) and is consistently animated across all devices.
 * EaseOut easing gives a more organic "heartbeat" feel than linear.
 */
@Composable
private fun MiningPulseAnimation(color: Color) {
    val infiniteTransition = rememberInfiniteTransition(label = "mining_pulse")
    val alpha by infiniteTransition.animateFloat(
        initialValue = 0.4f,
        targetValue = 0.0f,
        animationSpec = infiniteRepeatable(
            animation = tween(
                durationMillis = 1500,
                easing = androidx.compose.animation.core.EaseOut,
            ),
            repeatMode = RepeatMode.Restart,
        ),
        label = "pulse_alpha",
    )
    val scale by infiniteTransition.animateFloat(
        initialValue = 1.0f,
        targetValue = 1.8f,
        animationSpec = infiniteRepeatable(
            animation = tween(
                durationMillis = 1500,
                easing = androidx.compose.animation.core.EaseOut,
            ),
            repeatMode = RepeatMode.Restart,
        ),
        label = "pulse_scale",
    )

    Canvas(
        modifier = Modifier
            .size(80.dp)
            .graphicsLayer {
                this.alpha = alpha
                scaleX = scale
                scaleY = scale
            },
    ) {
        drawCircle(
            color = color,
            radius = size.minDimension / 2,
        )
    }
}

/**
 * Animated horizontal activity bar showing mining is active.
 * A bright segment sweeps back and forth continuously.
 */
@Composable
private fun MiningActivityBar(color: Color) {
    val infiniteTransition = rememberInfiniteTransition(label = "mining_bar")
    val progress by infiniteTransition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 2000, easing = LinearEasing),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "bar_sweep",
    )

    Canvas(
        modifier = Modifier
            .fillMaxWidth()
            .height(4.dp)
            .padding(horizontal = 24.dp),
    ) {
        // Background track
        drawRoundRect(
            color = color.copy(alpha = 0.15f),
            size = size,
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(2.dp.toPx()),
        )
        // Sweeping active segment (20% width)
        val segmentWidth = size.width * 0.2f
        val xOffset = progress * (size.width - segmentWidth)
        drawRoundRect(
            color = color,
            topLeft = androidx.compose.ui.geometry.Offset(xOffset, 0f),
            size = androidx.compose.ui.geometry.Size(segmentWidth, size.height),
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(2.dp.toPx()),
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
// Consensus & Network Info Card
// ============================================================================

@Composable
private fun ConsensusInfoCard(
    peerCount: Int,
    blockHeight: Long,
    currentSlot: Long,
    isCommitteeMember: Boolean,
    blocksProduced: Long,
    syncStatus: String,
    isNetworkRunning: Boolean,
) {
    val statusColor = if (isNetworkRunning) SignalGreen else MaterialTheme.colorScheme.outline

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
        ),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(
                    modifier = Modifier
                        .size(10.dp)
                        .padding(end = 0.dp),
                ) {
                    Canvas(modifier = Modifier.size(10.dp)) {
                        drawCircle(color = statusColor)
                    }
                }
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = "Network & Consensus",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
            }

            Spacer(modifier = Modifier.height(12.dp))

            // Stats grid: 3 columns x 2 rows
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                StatItem(value = "$peerCount", label = "Peers")
                StatItem(value = "$blockHeight", label = "Block Height")
                StatItem(value = "$currentSlot", label = "Slot")
            }

            Spacer(modifier = Modifier.height(8.dp))

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                StatItem(value = "$blocksProduced", label = "Blocks Produced")
                StatItem(
                    value = if (isCommitteeMember) "Yes" else "No",
                    label = "Committee",
                    valueColor = if (isCommitteeMember) SignalGreen else MaterialTheme.colorScheme.outline,
                )
                StatItem(
                    value = syncStatus.replaceFirstChar { it.uppercase() },
                    label = "Sync",
                    valueColor = when {
                        syncStatus == "synced" -> SignalGreen
                        syncStatus.startsWith("syncing") -> AmberGold
                        syncStatus.startsWith("behind") -> Color(0xFFE57373)
                        else -> MaterialTheme.colorScheme.outline
                    },
                )
            }
        }
    }
}

@Composable
private fun StatItem(
    value: String,
    label: String,
    valueColor: Color = MaterialTheme.colorScheme.onSurface,
) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(
            text = value,
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            color = valueColor,
        )
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )
    }
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
                        status.batteryPercent >= 80 -> SignalGreen
                        status.batteryPercent >= 50 -> AmberGold
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
                        progress = status.batteryPercent / 100f,
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
                    tint = if (status.isPluggedIn) SignalGreen else MaterialTheme.colorScheme.outline,
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
                val validColor = if (polStatus.isValidToday) SignalGreen else AmberGold
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
            tint = if (isMet) SignalGreen else MaterialTheme.colorScheme.outline.copy(alpha = 0.5f),
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
                if (score == 0) {
                    Text(
                        text = "Not yet calculated",
                        style = MaterialTheme.typography.headlineSmall,
                        fontWeight = FontWeight.Bold,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    )
                } else {
                    Text(
                        text = "$score",
                        style = MaterialTheme.typography.headlineMedium,
                        fontWeight = FontWeight.Bold,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
                Text(
                    text = " / 100",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    modifier = Modifier.padding(bottom = 4.dp),
                )
            }
            Spacer(modifier = Modifier.height(4.dp))
            LinearProgressIndicator(
                progress = score / 100f,
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

// ============================================================================
// Staking Card
// ============================================================================

@Composable
private fun StakingCard(
    stakeInfo: StakeInfo,
    onStakeClick: () -> Unit,
    onUnstakeClick: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Staking",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                // WHY: Minimum stake is a prerequisite for mining (three-pillar consensus).
                // Show a clear pass/fail indicator so the user knows immediately.
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        imageVector = if (stakeInfo.meetsMinimum) Icons.Default.Check else Icons.Default.Close,
                        contentDescription = if (stakeInfo.meetsMinimum) "Minimum met" else "Minimum not met",
                        tint = if (stakeInfo.meetsMinimum) SignalGreen else AlertRed,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        text = if (stakeInfo.meetsMinimum) "Minimum Met" else "Below Minimum",
                        style = MaterialTheme.typography.labelLarge,
                        fontWeight = FontWeight.SemiBold,
                        color = if (stakeInfo.meetsMinimum) SignalGreen else AlertRed,
                    )
                }
            }

            Spacer(modifier = Modifier.height(12.dp))

            // Node Stake
            StakeInfoRow("Node Stake", stakeInfo.nodeStakeLux)

            // WHY: Overflow is the amount above the per-node cap that flows to the
            // Network Security Pool. Users should see this so they understand the cap.
            StakeInfoRow("Overflow", stakeInfo.overflowAmountLux)

            StakeInfoRow("Total Committed", stakeInfo.totalCommittedLux)

            Spacer(modifier = Modifier.height(16.dp))

            // Stake / Unstake buttons
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Button(
                    onClick = onStakeClick,
                    modifier = Modifier.weight(1f),
                ) {
                    Text("Stake")
                }
                OutlinedButton(
                    onClick = onUnstakeClick,
                    modifier = Modifier.weight(1f),
                ) {
                    Text("Unstake")
                }
            }
        }
    }
}

@Composable
private fun StakeInfoRow(label: String, amountLux: Long) {
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

// ============================================================================
// Stake / Unstake Amount Dialog
// ============================================================================

@Composable
private fun StakeAmountDialog(
    title: String,
    confirmLabel: String,
    errorMessage: String?,
    onConfirm: (amountGrat: Long) -> Unit,
    onDismiss: () -> Unit,
) {
    var amountText by remember { mutableStateOf("") }

    // WHY: Parse as Long (whole GRAT) because the minimum stake and cap are
    // whole-number values. Fractional staking can be added later if needed.
    val parsedAmount = amountText.toLongOrNull()
    val isValid = parsedAmount != null && parsedAmount > 0

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column {
                OutlinedTextField(
                    value = amountText,
                    onValueChange = { amountText = it },
                    label = { Text("Amount (GRAT)") },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    isError = errorMessage != null,
                )
                if (errorMessage != null) {
                    Spacer(modifier = Modifier.height(4.dp))
                    Text(
                        text = errorMessage,
                        style = MaterialTheme.typography.bodySmall,
                        color = AlertRed,
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { if (isValid) onConfirm(parsedAmount!!) },
                enabled = isValid,
            ) {
                Text(confirmLabel)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}
