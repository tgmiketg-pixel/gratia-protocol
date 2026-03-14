package io.gratia.app.ui

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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Divider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

// ============================================================================
// SettingsScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    viewModel: SettingsViewModel = viewModel(),
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
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
            SettingsContent(
                state = state,
                onShowExportSeed = { viewModel.showExportSeedConfirmation() },
                onShowStakeDialog = { viewModel.showStakeDialog() },
                onShowUnstakeDialog = { viewModel.showUnstakeDialog() },
                onLocationGranularity = { viewModel.setLocationGranularity(it) },
                onCameraHashToggle = { viewModel.setCameraHashEnabled(it) },
                onMicFingerprintToggle = { viewModel.setMicrophoneFingerprintEnabled(it) },
                onInheritanceToggle = { viewModel.setInheritanceEnabled(it) },
                onShowBeneficiaryDialog = { viewModel.showBeneficiaryDialog() },
                modifier = Modifier.padding(padding),
            )
        }

        // Export seed phrase confirmation
        if (state.showExportSeedConfirmation) {
            ExportSeedConfirmationDialog(
                onConfirm = { viewModel.exportSeedPhrase() },
                onDismiss = { viewModel.hideExportSeedConfirmation() },
            )
        }

        // Stake dialog
        if (state.showStakeDialog) {
            AmountDialog(
                title = "Stake GRAT",
                description = "Enter the amount to stake. Stakes above the per-node cap (1,000 GRAT) overflow to the Network Security Pool.",
                actionLabel = "Stake",
                onAction = { viewModel.stake(it) },
                onDismiss = { viewModel.hideStakeDialog() },
            )
        }

        // Unstake dialog
        if (state.showUnstakeDialog) {
            AmountDialog(
                title = "Unstake GRAT",
                description = "Enter the amount to unstake. Overflow stake is removed first. Subject to cooldown period.",
                actionLabel = "Unstake",
                onAction = { viewModel.unstake(it) },
                onDismiss = { viewModel.hideUnstakeDialog() },
            )
        }

        // Beneficiary address dialog
        if (state.showBeneficiaryDialog) {
            BeneficiaryDialog(
                currentAddress = state.beneficiaryAddress,
                onSave = { viewModel.setBeneficiaryAddress(it) },
                onDismiss = { viewModel.hideBeneficiaryDialog() },
            )
        }
    }
}

@Composable
private fun SettingsContent(
    state: SettingsUiState,
    onShowExportSeed: () -> Unit,
    onShowStakeDialog: () -> Unit,
    onShowUnstakeDialog: () -> Unit,
    onLocationGranularity: (LocationGranularity) -> Unit,
    onCameraHashToggle: (Boolean) -> Unit,
    onMicFingerprintToggle: (Boolean) -> Unit,
    onInheritanceToggle: (Boolean) -> Unit,
    onShowBeneficiaryDialog: () -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        modifier = modifier.fillMaxSize(),
    ) {
        // Wallet section
        item {
            WalletSettingsSection(onShowExportSeed)
        }

        // Staking section
        item {
            StakingSection(
                stakeInfo = state.stakeInfo,
                onStake = onShowStakeDialog,
                onUnstake = onShowUnstakeDialog,
            )
        }

        // Privacy section
        item {
            PrivacySection(
                locationGranularity = state.locationGranularity,
                cameraHashEnabled = state.cameraHashEnabled,
                micFingerprintEnabled = state.microphoneFingerprintEnabled,
                onLocationGranularity = onLocationGranularity,
                onCameraHashToggle = onCameraHashToggle,
                onMicFingerprintToggle = onMicFingerprintToggle,
            )
        }

        // Inheritance section
        item {
            InheritanceSection(
                enabled = state.inheritanceEnabled,
                beneficiaryAddress = state.beneficiaryAddress,
                onToggle = onInheritanceToggle,
                onEditBeneficiary = onShowBeneficiaryDialog,
            )
        }

        // About section
        item {
            AboutSection(
                appVersion = state.appVersion,
                nodeId = state.nodeId,
                participationDays = state.participationDays,
            )
        }
    }
}

// ============================================================================
// Section: Wallet
// ============================================================================

@Composable
private fun WalletSettingsSection(onShowExportSeed: () -> Unit) {
    SettingsSection(title = "Wallet") {
        Text(
            text = "Recovery Options",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Medium,
        )
        Spacer(modifier = Modifier.height(4.dp))
        Text(
            text = "Your wallet is secured by the device's secure enclave and your Proof of Life behavioral profile. If you lose this device, recovery uses PoL behavioral matching over 7-14 days on a new device.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )

        Spacer(modifier = Modifier.height(12.dp))

        OutlinedButton(
            onClick = onShowExportSeed,
            colors = ButtonDefaults.outlinedButtonColors(
                contentColor = MaterialTheme.colorScheme.error,
            ),
        ) {
            Icon(
                Icons.Default.Key,
                contentDescription = null,
                modifier = Modifier.padding(end = 8.dp),
            )
            Text("Export Seed Phrase")
        }

        Text(
            text = "Optional. Store securely if exported. This is NOT the recommended recovery method.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
        )
    }
}

// ============================================================================
// Section: Staking
// ============================================================================

@Composable
private fun StakingSection(
    stakeInfo: StakeInfo?,
    onStake: () -> Unit,
    onUnstake: () -> Unit,
) {
    SettingsSection(title = "Staking") {
        if (stakeInfo != null) {
            // Current stake display
            StakingRow("Effective stake", formatGrat(stakeInfo.nodeStakeLux) + " GRAT")
            if (stakeInfo.overflowAmountLux > 0) {
                StakingRow("Overflow to pool", formatGrat(stakeInfo.overflowAmountLux) + " GRAT")
            }
            StakingRow("Total committed", formatGrat(stakeInfo.totalCommittedLux) + " GRAT")
            StakingRow(
                "Minimum met",
                if (stakeInfo.meetsMinimum) "Yes" else "No",
            )
        } else {
            Text(
                text = "No stake active",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
            )
        }

        Spacer(modifier = Modifier.height(12.dp))

        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Button(onClick = onStake) {
                Text("Stake")
            }
            OutlinedButton(onClick = onUnstake) {
                Text("Unstake")
            }
        }
    }
}

@Composable
private fun StakingRow(label: String, value: String) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
        )
        Text(
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

// ============================================================================
// Section: Privacy
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PrivacySection(
    locationGranularity: LocationGranularity,
    cameraHashEnabled: Boolean,
    micFingerprintEnabled: Boolean,
    onLocationGranularity: (LocationGranularity) -> Unit,
    onCameraHashToggle: (Boolean) -> Unit,
    onMicFingerprintToggle: (Boolean) -> Unit,
) {
    SettingsSection(title = "Privacy") {
        Text(
            text = "All sensor data is processed on-device. Raw data never leaves your phone. Zero-knowledge proofs are used for all attestations.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )

        Spacer(modifier = Modifier.height(12.dp))

        // Location granularity dropdown
        Text(
            text = "Location Granularity",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Medium,
        )
        Spacer(modifier = Modifier.height(4.dp))

        var expanded by remember { mutableStateOf(false) }
        ExposedDropdownMenuBox(
            expanded = expanded,
            onExpandedChange = { expanded = !expanded },
        ) {
            OutlinedTextField(
                value = locationGranularity.label,
                onValueChange = {},
                readOnly = true,
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                modifier = Modifier
                    .fillMaxWidth()
                    .menuAnchor(),
            )
            ExposedDropdownMenu(
                expanded = expanded,
                onDismissRequest = { expanded = false },
            ) {
                LocationGranularity.entries.forEach { option ->
                    DropdownMenuItem(
                        text = { Text(option.label) },
                        onClick = {
                            onLocationGranularity(option)
                            expanded = false
                        },
                    )
                }
            }
        }

        Spacer(modifier = Modifier.height(16.dp))

        // Optional sensor toggles
        Text(
            text = "Optional Sensors (Enhanced)",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Medium,
        )
        Spacer(modifier = Modifier.height(4.dp))

        SettingsToggle(
            label = "Camera environment hash",
            description = "Contributes to Presence Score (+4). Only a hash of the environment is used, never images.",
            checked = cameraHashEnabled,
            onCheckedChange = onCameraHashToggle,
        )

        SettingsToggle(
            label = "Microphone ambient fingerprint",
            description = "Contributes to Presence Score (+4). Only an acoustic fingerprint is used, never audio content.",
            checked = micFingerprintEnabled,
            onCheckedChange = onMicFingerprintToggle,
        )
    }
}

// ============================================================================
// Section: Inheritance
// ============================================================================

@Composable
private fun InheritanceSection(
    enabled: Boolean,
    beneficiaryAddress: String,
    onToggle: (Boolean) -> Unit,
    onEditBeneficiary: () -> Unit,
) {
    SettingsSection(title = "Inheritance") {
        Text(
            text = "Designate a beneficiary wallet that receives your funds if the 365-day dead-man switch triggers. Your daily Proof of Life activity resets the timer automatically.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )

        Spacer(modifier = Modifier.height(8.dp))

        SettingsToggle(
            label = "Enable dead-man switch",
            description = "365-day inactivity timer",
            checked = enabled,
            onCheckedChange = onToggle,
        )

        if (enabled) {
            Spacer(modifier = Modifier.height(8.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = "Beneficiary",
                        style = MaterialTheme.typography.bodyMedium,
                        fontWeight = FontWeight.Medium,
                    )
                    Text(
                        text = if (beneficiaryAddress.isNotEmpty()) {
                            truncateAddress(beneficiaryAddress)
                        } else {
                            "Not set"
                        },
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                }
                OutlinedButton(onClick = onEditBeneficiary) {
                    Text(if (beneficiaryAddress.isNotEmpty()) "Change" else "Set")
                }
            }
        }
    }
}

// ============================================================================
// Section: About
// ============================================================================

@Composable
private fun AboutSection(
    appVersion: String,
    nodeId: String,
    participationDays: Long,
) {
    SettingsSection(title = "About") {
        AboutRow("App version", appVersion)
        AboutRow("Node ID", truncateAddress(nodeId))
        AboutRow("Participation", "$participationDays days")
    }
}

@Composable
private fun AboutRow(label: String, value: String) {
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
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = FontFamily.Monospace,
        )
    }
}

// ============================================================================
// Shared Components
// ============================================================================

@Composable
private fun SettingsSection(
    title: String,
    content: @Composable () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surface,
        ),
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = title,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
            )
            Divider(modifier = Modifier.padding(vertical = 8.dp))
            content()
        }
    }
}

@Composable
private fun SettingsToggle(
    label: String,
    description: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = label,
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = description,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
            )
        }
        Spacer(modifier = Modifier.width(8.dp))
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
        )
    }
}

// ============================================================================
// Dialogs
// ============================================================================

@Composable
private fun ExportSeedConfirmationDialog(
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = {
            Icon(
                Icons.Default.Warning,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.error,
            )
        },
        title = { Text("Export Seed Phrase") },
        text = {
            Column {
                Text("Are you sure you want to export your seed phrase?")
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "This is NOT the recommended recovery method. The Proof of Life behavioral recovery is more secure. Only export your seed phrase if you understand the risks of storing it.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        },
        confirmButton = {
            Button(
                onClick = onConfirm,
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                ),
            ) {
                Text("I Understand, Export")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

@Composable
private fun AmountDialog(
    title: String,
    description: String,
    actionLabel: String,
    onAction: (amountLux: Long) -> Unit,
    onDismiss: () -> Unit,
) {
    var amountText by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column {
                Text(
                    text = description,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                )
                Spacer(modifier = Modifier.height(12.dp))
                OutlinedTextField(
                    value = amountText,
                    onValueChange = {
                        amountText = it
                        error = null
                    },
                    label = { Text("Amount (GRAT)") },
                    placeholder = { Text("0.00") },
                    isError = error != null,
                    supportingText = error?.let { { Text(it) } },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    val amount = amountText.toDoubleOrNull()
                    if (amount == null || amount <= 0) {
                        error = "Enter a valid amount"
                        return@Button
                    }
                    val lux = (amount * 1_000_000).toLong()
                    onAction(lux)
                },
            ) {
                Text(actionLabel)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

@Composable
private fun BeneficiaryDialog(
    currentAddress: String,
    onSave: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var address by remember { mutableStateOf(currentAddress) }
    var error by remember { mutableStateOf<String?>(null) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Set Beneficiary") },
        text = {
            Column {
                Text(
                    text = "Enter the wallet address that should receive your funds after 365 days of inactivity.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                )
                Spacer(modifier = Modifier.height(12.dp))
                OutlinedTextField(
                    value = address,
                    onValueChange = {
                        address = it
                        error = null
                    },
                    label = { Text("Beneficiary address") },
                    placeholder = { Text("grat:...") },
                    isError = error != null,
                    supportingText = error?.let { { Text(it) } },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    if (!address.startsWith("grat:") || address.length < 10) {
                        error = "Invalid address format"
                        return@Button
                    }
                    onSave(address)
                },
            ) {
                Text("Save")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}
