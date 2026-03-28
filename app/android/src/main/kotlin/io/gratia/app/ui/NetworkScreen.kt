package io.gratia.app.ui

import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bluetooth
import androidx.compose.material.icons.filled.CellTower
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.LinkOff
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Wifi
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Divider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import io.gratia.app.GratiaLogo
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import io.gratia.app.ui.theme.*

// ============================================================================
// NetworkScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NetworkScreen(
    viewModel: NetworkViewModel = viewModel(),
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    // WHY: Poll network status, peer count, and block height every 5 seconds
    // so the UI stays current even if the ViewModel's internal polling stalls
    // (e.g. after process death / recreation). This mirrors the polling pattern
    // used by WalletScreen and MiningScreen through their ViewModels.
    LaunchedEffect(Unit) {
        while (true) {
            kotlinx.coroutines.delay(5000)
            viewModel.refresh()
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = { GratiaLogo(modifier = Modifier.padding(start = 12.dp)) },
                title = { Text("Network") },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surface,
                ),
            )
        },
    ) { padding ->
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            // Network control card
            item {
                NetworkControlCard(
                    isRunning = state.isNetworkRunning,
                    peerCount = state.peerCount,
                    listenAddress = state.listenAddress,
                    isLoading = state.isLoading,
                    onStart = { viewModel.startNetwork() },
                    onStop = { viewModel.stopNetwork() },
                )
            }

            // Consensus card (only show when network is running)
            if (state.isNetworkRunning) {
                item {
                    ConsensusCard(
                        consensusState = state.consensusState,
                        currentSlot = state.currentSlot,
                        currentHeight = state.currentHeight,
                        isCommitteeMember = state.isCommitteeMember,
                        blocksProduced = state.blocksProduced,
                        onStartConsensus = { viewModel.startConsensus() },
                        onStopConsensus = { viewModel.stopConsensus() },
                    )
                }
            }

            // Mesh transport card (Phase 3 — Bluetooth + Wi-Fi Direct)
            if (state.isNetworkRunning) {
                item {
                    MeshTransportCard(
                        meshStatus = state.meshStatus,
                        isLoading = state.isMeshLoading,
                        onStart = { viewModel.startMesh() },
                        onStop = { viewModel.stopMesh() },
                    )
                }
            }

            // Error message
            if (state.errorMessage != null) {
                item {
                    Card(
                        modifier = Modifier.fillMaxWidth(),
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.errorContainer,
                        ),
                    ) {
                        Text(
                            text = state.errorMessage ?: "",
                            modifier = Modifier.padding(16.dp),
                            color = MaterialTheme.colorScheme.onErrorContainer,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
            }

            // Event log
            if (state.recentEvents.isNotEmpty()) {
                item {
                    Text(
                        text = "Event Log",
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                }

                items(state.recentEvents) { event ->
                    Text(
                        text = event,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                        modifier = Modifier.padding(vertical = 2.dp),
                    )
                }
            }
        }
    }
}

// ============================================================================
// Network Control Card
// ============================================================================

@Composable
private fun NetworkControlCard(
    isRunning: Boolean,
    peerCount: Int,
    listenAddress: String?,
    isLoading: Boolean,
    onStart: () -> Unit,
    onStop: () -> Unit,
) {
    val statusColor = if (isRunning) SignalGreen else MaterialTheme.colorScheme.outline

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = statusColor.copy(alpha = 0.12f),
        ),
    ) {
        Column(
            modifier = Modifier.padding(20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Icon(
                imageVector = Icons.Default.CellTower,
                contentDescription = null,
                tint = statusColor,
                modifier = Modifier.size(48.dp),
            )

            Spacer(modifier = Modifier.height(12.dp))

            Text(
                text = if (isRunning) "Network Active" else "Network Offline",
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
                color = statusColor,
            )

            Spacer(modifier = Modifier.height(8.dp))

            if (isRunning) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(24.dp),
                ) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(
                            text = "$peerCount",
                            style = MaterialTheme.typography.headlineMedium,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.primary,
                        )
                        Text(
                            text = "Peers",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                        )
                    }
                }

                if (listenAddress != null) {
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = listenAddress,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    )
                }
            }

            Spacer(modifier = Modifier.height(16.dp))

            if (isRunning) {
                OutlinedButton(
                    onClick = onStop,
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) {
                    Icon(Icons.Default.LinkOff, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Stop Network")
                }
            } else {
                Button(
                    onClick = onStart,
                    enabled = !isLoading,
                ) {
                    Icon(Icons.Default.Link, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(if (isLoading) "Starting..." else "Start Network")
                }
            }
        }
    }
}

// ============================================================================
// Consensus Card
// ============================================================================

@Composable
private fun ConsensusCard(
    consensusState: String,
    currentSlot: Long,
    currentHeight: Long,
    isCommitteeMember: Boolean,
    blocksProduced: Long,
    onStartConsensus: () -> Unit,
    onStopConsensus: () -> Unit,
) {
    val isActive = consensusState != "stopped"
    val stateColor = when (consensusState) {
        "active" -> SignalGreen
        "producing" -> AmberGold
        "syncing" -> DarkAmber
        else -> MaterialTheme.colorScheme.outline
    }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Consensus",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    text = consensusState.replaceFirstChar { it.uppercase() },
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = stateColor,
                )
            }

            if (isActive) {
                Spacer(modifier = Modifier.height(16.dp))

                // Stats grid
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                ) {
                    StatColumn("Height", "$currentHeight")
                    StatColumn("Slot", "$currentSlot")
                    StatColumn("Produced", "$blocksProduced")
                }

                Spacer(modifier = Modifier.height(12.dp))
                Divider()
                Spacer(modifier = Modifier.height(12.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = "Committee member",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Text(
                        text = if (isCommitteeMember) "Yes" else "No",
                        style = MaterialTheme.typography.bodyMedium,
                        fontWeight = FontWeight.SemiBold,
                        color = if (isCommitteeMember) SignalGreen else MaterialTheme.colorScheme.outline,
                    )
                }
            }

            Spacer(modifier = Modifier.height(16.dp))

            if (isActive) {
                OutlinedButton(
                    onClick = onStopConsensus,
                    modifier = Modifier.fillMaxWidth(),
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) {
                    Icon(Icons.Default.Stop, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Stop Consensus")
                }
            } else {
                Button(
                    onClick = onStartConsensus,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Icon(Icons.Default.PlayArrow, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Start Consensus")
                }
            }
        }
    }
}

@Composable
private fun StatColumn(label: String, value: String) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(
            text = value,
            style = MaterialTheme.typography.headlineSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.primary,
        )
        Text(
            text = label,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )
    }
}

// ============================================================================
// Mesh Transport Card (Phase 3)
// ============================================================================

@Composable
private fun MeshTransportCard(
    meshStatus: MeshStatus,
    isLoading: Boolean,
    onStart: () -> Unit,
    onStop: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Mesh Transport",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    text = if (meshStatus.enabled) "Active" else "Inactive",
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = if (meshStatus.enabled) SignalGreen else MaterialTheme.colorScheme.outline,
                )
            }

            Spacer(modifier = Modifier.height(4.dp))

            Text(
                text = "Bluetooth LE + Wi-Fi Direct for offline peer-to-peer connectivity.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
            )

            if (meshStatus.enabled) {
                Spacer(modifier = Modifier.height(16.dp))

                // Transport indicators
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            Icons.Default.Bluetooth,
                            contentDescription = null,
                            modifier = Modifier.size(16.dp),
                            tint = if (meshStatus.bluetoothActive) SignalGreen
                                   else MaterialTheme.colorScheme.outline,
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text(
                            text = "BLE",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (meshStatus.bluetoothActive) SignalGreen
                                    else MaterialTheme.colorScheme.outline,
                        )
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            Icons.Default.Wifi,
                            contentDescription = null,
                            modifier = Modifier.size(16.dp),
                            tint = if (meshStatus.wifiDirectActive) SignalGreen
                                   else MaterialTheme.colorScheme.outline,
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text(
                            text = "Wi-Fi Direct",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (meshStatus.wifiDirectActive) SignalGreen
                                    else MaterialTheme.colorScheme.outline,
                        )
                    }
                }

                Spacer(modifier = Modifier.height(12.dp))
                Divider()
                Spacer(modifier = Modifier.height(12.dp))

                // Peer stats
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                ) {
                    StatColumn("Mesh Peers", "${meshStatus.meshPeerCount}")
                    StatColumn("Bridge Peers", "${meshStatus.bridgePeerCount}")
                    StatColumn("Pending Relay", "${meshStatus.pendingRelayCount}")
                }
            }

            Spacer(modifier = Modifier.height(16.dp))

            if (meshStatus.enabled) {
                OutlinedButton(
                    onClick = onStop,
                    modifier = Modifier.fillMaxWidth(),
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) {
                    Icon(Icons.Default.Stop, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Stop Mesh")
                }
            } else {
                Button(
                    onClick = onStart,
                    modifier = Modifier.fillMaxWidth(),
                    enabled = !isLoading,
                ) {
                    Icon(Icons.Default.PlayArrow, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(if (isLoading) "Starting..." else "Start Mesh")
                }
            }
        }
    }
}

