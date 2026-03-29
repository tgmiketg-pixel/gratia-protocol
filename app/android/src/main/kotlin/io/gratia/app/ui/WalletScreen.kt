package io.gratia.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.CallMade
import androidx.compose.material.icons.automirrored.filled.CallReceived
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Warning
import io.gratia.app.GratiaLogo
import androidx.compose.material.icons.filled.QrCode
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Send
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Divider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SuggestionChip
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
import androidx.compose.ui.graphics.Color
import io.gratia.app.ui.theme.*
import androidx.compose.ui.graphics.asImageBitmap
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import android.graphics.Bitmap
import androidx.compose.foundation.Image
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter
import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.wrapContentSize
import androidx.core.content.ContextCompat
import androidx.compose.runtime.LaunchedEffect
import io.gratia.app.MainActivity
import io.gratia.app.security.AddressBook
import io.gratia.app.security.SecurityManager
import io.gratia.app.security.LockScreen
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

// ============================================================================
// WalletScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WalletScreen(
    viewModel: WalletViewModel = viewModel(),
    onNavigateToSettings: (() -> Unit)? = null,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val context = LocalContext.current

    // WHY: Track whether the user has exported their seed phrase. Persisted
    // via SharedPreferences so the backup banner disappears permanently after
    // the first successful export.
    val prefs = remember { context.getSharedPreferences("gratia_wallet", Context.MODE_PRIVATE) }
    var seedExported by remember { mutableStateOf(prefs.getBoolean("seed_exported", false)) }

    // WHY: Track scanner visibility and scanned address separately from
    // the ViewModel to keep scanner state local to this composable.
    var showScanner by remember { mutableStateOf(false) }
    var scannedAddress by remember { mutableStateOf<String?>(null) }

    // WHY: Observe the NFC-scanned address from MainActivity's reader mode.
    // When two phones tap together, the reader side receives the other phone's
    // wallet address via HCE and publishes it to this StateFlow. We consume it
    // here and open the send dialog pre-filled with the address.
    // WHY LaunchedEffect: Side effects (setting state, opening dialog) must not
    // run during composition — they must be triggered reactively when the value changes.
    val nfcAddress by MainActivity.nfcScannedAddress.collectAsStateWithLifecycle()
    LaunchedEffect(nfcAddress) {
        val addr = nfcAddress ?: return@LaunchedEffect
        scannedAddress = addr
        MainActivity.clearNfcScannedAddress()
        if (!state.showSendDialog) {
            viewModel.showSendDialog()
        }
    }

    // Camera permission launcher
    var cameraPermissionGranted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA)
                    == PackageManager.PERMISSION_GRANTED
        )
    }
    val cameraPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        cameraPermissionGranted = granted
        if (granted) {
            showScanner = true
        }
    }

    // WHY: When the scanner is active, show it full-screen on top of everything.
    // This avoids complex navigation — just overlay and dismiss.
    if (showScanner && cameraPermissionGranted) {
        QrScannerScreen(
            onQrCodeScanned = { qrContent ->
                scannedAddress = qrContent
                showScanner = false
                // WHY: Ensure the send dialog is visible so the scanned
                // address appears in the address field immediately.
                if (!state.showSendDialog) {
                    viewModel.showSendDialog()
                }
            },
            onDismiss = { showScanner = false },
        )
        return
    }

    // WHY: Connection status shows at a glance if the node is online and
    // how many peers it's connected to. Users need to know if their
    // transactions will actually propagate.
    val networkStatus = remember { mutableStateOf<io.gratia.app.bridge.NetworkStatus?>(null) }
    LaunchedEffect(Unit) {
        try {
            networkStatus.value = io.gratia.app.bridge.GratiaCoreManager.getNetworkStatus()
        } catch (_: Exception) {}
    }

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
                    "Wallet",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = androidx.compose.ui.text.font.FontWeight.Bold,
                    modifier = Modifier.weight(1f),
                )
                // Connection status chip
                networkStatus.value?.let { net ->
                    val chipColor = if (net.isRunning && net.peerCount > 0) {
                        io.gratia.app.ui.theme.SignalGreen
                    } else if (net.isRunning) {
                        io.gratia.app.ui.theme.AmberGold
                    } else {
                        MaterialTheme.colorScheme.error
                    }
                    val label = if (net.isRunning) {
                        "${net.peerCount} peer${if (net.peerCount != 1) "s" else ""}"
                    } else {
                        "Offline"
                    }
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .background(
                                chipColor.copy(alpha = 0.15f),
                                shape = androidx.compose.foundation.shape.RoundedCornerShape(12.dp),
                            )
                            .padding(horizontal = 10.dp, vertical = 4.dp),
                    ) {
                        Box(
                            modifier = Modifier
                                .size(8.dp)
                                .background(chipColor, shape = androidx.compose.foundation.shape.CircleShape),
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = label,
                            style = MaterialTheme.typography.labelSmall,
                            color = chipColor,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                }
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
            // WHY: Pull-to-refresh lets users manually update their balance
            // and transaction history without waiting for the 5-second poll.
            var isRefreshing by remember { mutableStateOf(false) }
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            ) {
                WalletContent(
                    state = state,
                    seedExported = seedExported,
                    onSendClick = { viewModel.showSendDialog() },
                    onReceiveClick = { viewModel.showReceiveDialog() },
                    onBackupClick = onNavigateToSettings ?: {},
                    onRestoreClick = { viewModel.showRestoreDialog() },
                    onRefresh = {
                        isRefreshing = true
                        viewModel.loadWalletData()
                        isRefreshing = false
                    },
                    isRefreshing = isRefreshing,
                    isOffline = networkStatus.value?.isRunning == false,
                )
            }
        }

        // Restore wallet dialog (from WalletScreen empty state link)
        if (state.showRestoreDialog) {
            RestoreWalletInlineDialog(
                error = state.restoreError,
                onConfirm = { seedHex -> viewModel.importSeedPhrase(seedHex) },
                onDismiss = { viewModel.hideRestoreDialog() },
            )
        }

        // Restore success dialog
        state.restoredAddress?.let { address ->
            AlertDialog(
                onDismissRequest = { viewModel.clearRestoredAddress() },
                title = { Text("Wallet Restored") },
                text = {
                    Column {
                        Text("Your wallet has been successfully restored.")
                        Spacer(modifier = Modifier.height(8.dp))
                        Text(
                            text = address,
                            style = MaterialTheme.typography.bodySmall,
                            fontFamily = FontFamily.Monospace,
                        )
                    }
                },
                confirmButton = {
                    Button(onClick = { viewModel.clearRestoredAddress() }) {
                        Text("Done")
                    }
                },
            )
        }

        // Send dialog with transaction auth gate
        // WHY: pendingSend holds the address+amount while the user authenticates.
        // The send only executes after successful auth, preventing unauthorized transfers.
        var pendingSend by remember { mutableStateOf<Pair<String, Long>?>(null) }
        var showTxAuth by remember { mutableStateOf(false) }

        if (state.showSendDialog) {
            SendDialog(
                initialAddress = scannedAddress,
                balanceLux = state.walletInfo?.balanceLux ?: 0L,
                onDismiss = {
                    viewModel.hideSendDialog()
                    scannedAddress = null
                },
                onSend = { address, amount ->
                    if (SecurityManager.shouldAuthForTransaction()) {
                        pendingSend = Pair(address, amount)
                        showTxAuth = true
                        viewModel.hideSendDialog()
                    } else {
                        viewModel.sendTransfer(address, amount)
                        scannedAddress = null
                    }
                },
                onScanClick = {
                    if (cameraPermissionGranted) {
                        showScanner = true
                    } else {
                        cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
                    }
                },
            )
        }

        // Transaction authentication overlay
        if (showTxAuth && pendingSend != null) {
            val lockMethod = SecurityManager.lockMethod
            if (lockMethod == SecurityManager.LockMethod.BIOMETRIC ||
                lockMethod == SecurityManager.LockMethod.DEVICE_CREDENTIAL) {
                // Trigger biometric prompt via Activity
                val activity = context as? MainActivity
                LaunchedEffect(Unit) {
                    activity?.showBiometricPrompt(
                        title = "Confirm Transaction",
                        subtitle = "Authenticate to send GRAT",
                    ) {
                        pendingSend?.let { (addr, amt) ->
                            viewModel.sendTransfer(addr, amt)
                        }
                        pendingSend = null
                        showTxAuth = false
                        scannedAddress = null
                    }
                }
            } else {
                // PIN or Pattern auth overlay
                androidx.compose.material3.AlertDialog(
                    onDismissRequest = {
                        showTxAuth = false
                        pendingSend = null
                    },
                    confirmButton = {},
                    title = {
                        Text(
                            "Confirm Transaction",
                            fontWeight = FontWeight.Bold,
                        )
                    },
                    text = {
                        when (lockMethod) {
                            SecurityManager.LockMethod.PIN -> {
                                io.gratia.app.security.PinEntry(
                                    onPinComplete = {
                                        pendingSend?.let { (addr, amt) ->
                                            viewModel.sendTransfer(addr, amt)
                                        }
                                        pendingSend = null
                                        showTxAuth = false
                                        scannedAddress = null
                                    },
                                )
                            }
                            SecurityManager.LockMethod.PATTERN -> {
                                io.gratia.app.security.PatternLock(
                                    onPatternComplete = {
                                        pendingSend?.let { (addr, amt) ->
                                            viewModel.sendTransfer(addr, amt)
                                        }
                                        pendingSend = null
                                        showTxAuth = false
                                        scannedAddress = null
                                    },
                                )
                            }
                            else -> {}
                        }
                    },
                )
            }
        }

        // Receive dialog
        if (state.showReceiveDialog) {
            ReceiveDialog(
                address = state.walletInfo?.address ?: "",
                onDismiss = { viewModel.hideReceiveDialog() },
            )
        }
    }
}

@Composable
private fun WalletContent(
    state: WalletUiState,
    seedExported: Boolean,
    onSendClick: () -> Unit,
    onReceiveClick: () -> Unit,
    onBackupClick: () -> Unit,
    onRestoreClick: () -> Unit,
    onRefresh: (() -> Unit)? = null,
    isRefreshing: Boolean = false,
    isOffline: Boolean = false,
) {
    val wallet = state.walletInfo ?: run {
        // WHY: On fresh install the wallet info may be null briefly while the
        // Rust core initialises key generation. Show a loading hint instead of
        // a blank screen so the user knows the app is working.
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 48.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = "Loading wallet...",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                textAlign = TextAlign.Center,
            )
        }
        return
    }

    // WHY: Track which transaction the user tapped to show its detail dialog.
    // Null means no dialog is showing.
    var selectedTransaction by remember { mutableStateOf<TransactionInfo?>(null) }

    // Transaction detail dialog
    selectedTransaction?.let { tx ->
        TransactionDetailDialog(
            tx = tx,
            onDismiss = { selectedTransaction = null },
        )
    }

    LazyColumn(
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        // Offline banner — shown when the network layer is not running
        if (isOffline) {
            item {
                OfflineBanner()
            }
        }

        // Balance card
        item {
            BalanceCard(
                wallet = wallet,
                onSendClick = onSendClick,
                onReceiveClick = onReceiveClick,
            )
        }

        // Backup reminder banner — shown only if seed phrase has NOT been exported
        if (!seedExported) {
            item {
                BackupReminderBanner(onClick = onBackupClick)
            }
        }

        // Transaction history header with refresh
        item {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Recent Transactions",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                // Export CSV button
                val exportContext = LocalContext.current
                IconButton(
                    onClick = {
                        CsvExporter.shareTransactions(exportContext, state.transactions)
                    },
                ) {
                    Icon(
                        imageVector = Icons.Default.Share,
                        contentDescription = "Export CSV",
                        modifier = Modifier.size(20.dp),
                    )
                }
                if (onRefresh != null) {
                    IconButton(onClick = onRefresh) {
                        if (isRefreshing) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(20.dp),
                                strokeWidth = 2.dp,
                            )
                        } else {
                            Icon(
                                imageVector = Icons.Default.Refresh,
                                contentDescription = "Refresh",
                                modifier = Modifier.size(20.dp),
                            )
                        }
                    }
                }
            }
        }

        // Transaction list or empty state
        if (state.transactions.isEmpty()) {
            item {
                EmptyTransactionState()
            }

            // WHY: If the wallet is empty (0 balance, 0 transactions), offer a
            // "Restore existing wallet?" link. This helps users who reinstalled
            // the app and want to restore from a backup without digging through
            // Settings first.
            if (wallet.balanceLux == 0L) {
                item {
                    TextButton(
                        onClick = onRestoreClick,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Icon(
                            Icons.Default.Key,
                            contentDescription = null,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        Text("Restore existing wallet?")
                    }
                }
            }
        } else {
            items(state.transactions, key = { it.hashHex }) { tx ->
                TransactionRow(
                    tx = tx,
                    onClick = { selectedTransaction = tx },
                )
            }
        }
    }
}

@Composable
private fun BalanceCard(
    wallet: WalletInfo,
    onSendClick: () -> Unit,
    onReceiveClick: () -> Unit,
) {
    val context = LocalContext.current
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.primaryContainer,
        ),
    ) {
        Column(
            modifier = Modifier.padding(20.dp),
        ) {
            // Address row
            Row(
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = truncateAddress(wallet.address),
                    style = MaterialTheme.typography.bodyMedium,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onPrimaryContainer.copy(alpha = 0.7f),
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    onClick = {
                        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        val clip = ClipData.newPlainText("Gratia Address", wallet.address)
                        clipboard.setPrimaryClip(clip)
                    },
                    modifier = Modifier.size(32.dp),
                ) {
                    Icon(
                        Icons.Default.ContentCopy,
                        contentDescription = "Copy address",
                        modifier = Modifier.size(18.dp),
                        tint = MaterialTheme.colorScheme.onPrimaryContainer.copy(alpha = 0.7f),
                    )
                }
            }

            Spacer(modifier = Modifier.height(12.dp))

            // Balance
            Text(
                text = "${formatGrat(wallet.balanceLux)} GRAT",
                style = MaterialTheme.typography.headlineLarge,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onPrimaryContainer,
            )

            // Lux conversion
            Text(
                text = "${"%,d".format(wallet.balanceLux)} Lux",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onPrimaryContainer.copy(alpha = 0.6f),
            )

            Spacer(modifier = Modifier.height(20.dp))

            // Action buttons
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Button(
                    onClick = onSendClick,
                    modifier = Modifier.weight(1f),
                ) {
                    Icon(
                        Icons.Default.Send,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Send")
                }
                OutlinedButton(
                    onClick = onReceiveClick,
                    modifier = Modifier.weight(1f),
                ) {
                    Icon(
                        Icons.Default.QrCode,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("Receive")
                }
            }
        }
    }
}

@Composable
private fun TransactionRow(
    tx: TransactionInfo,
    onClick: () -> Unit = {},
) {
    val isReceived = tx.direction == "received"
    val directionIcon = if (isReceived) {
        Icons.AutoMirrored.Filled.CallReceived
    } else {
        Icons.AutoMirrored.Filled.CallMade
    }
    val directionColor = if (isReceived) {
        SignalGreen
    } else {
        MaterialTheme.colorScheme.error
    }
    val amountPrefix = if (isReceived) "+" else "-"
    val dateFormat = remember { SimpleDateFormat("MMM d, HH:mm", Locale.getDefault()) }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // Direction icon
            Icon(
                directionIcon,
                contentDescription = tx.direction,
                tint = directionColor,
                modifier = Modifier.size(24.dp),
            )

            Spacer(modifier = Modifier.width(12.dp))

            // Address and timestamp
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = tx.counterparty?.let { truncateAddress(it) } ?: "Mining reward",
                    style = MaterialTheme.typography.bodyMedium,
                    fontFamily = FontFamily.Monospace,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = dateFormat.format(Date(tx.timestampMillis)),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                )
            }

            Spacer(modifier = Modifier.width(8.dp))

            // Amount and status
            Column(horizontalAlignment = Alignment.End) {
                Text(
                    text = "$amountPrefix${formatGrat(tx.amountLux)} GRAT",
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                    color = directionColor,
                )
                StatusChip(tx.status)
            }
        }
    }
}

// ============================================================================
// Transaction Detail Dialog
// ============================================================================

/**
 * Full-screen detail dialog for a single transaction.
 *
 * WHY: Users need to inspect transaction details (hash for verification,
 * full addresses for auditing, exact Lux amounts) without leaving the
 * wallet screen. Copy buttons let them paste hashes/addresses into a
 * block explorer or share with counterparties.
 */
@Composable
private fun TransactionDetailDialog(
    tx: TransactionInfo,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val clipboardManager = LocalClipboardManager.current
    val dateFormat = remember { SimpleDateFormat("MMM d, yyyy  HH:mm:ss", Locale.getDefault()) }

    val isReceived = tx.direction == "received"
    val isMiningReward = tx.counterparty == null
    val typeLabel = when {
        isMiningReward -> "Mining Reward"
        isReceived -> "Received"
        else -> "Sent"
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text(
                text = "Transaction Details",
                fontWeight = FontWeight.Bold,
            )
        },
        text = {
            Column(
                verticalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                // Type
                DetailRow(label = "Type", value = typeLabel)

                Divider()

                // Amount in GRAT
                val amountPrefix = if (isReceived || isMiningReward) "+" else "-"
                DetailRow(
                    label = "Amount",
                    value = "$amountPrefix${formatGrat(tx.amountLux)} GRAT",
                    valueColor = if (isReceived || isMiningReward) SignalGreen else MaterialTheme.colorScheme.error,
                )

                // Amount in Lux
                DetailRow(
                    label = "Amount (Lux)",
                    value = "%,d".format(tx.amountLux),
                )

                Divider()

                // Transaction hash with copy
                DetailRowWithCopy(
                    label = "Transaction Hash",
                    value = tx.hashHex,
                    displayValue = truncateAddress(tx.hashHex, prefixLen = 12, suffixLen = 8),
                    clipboardManager = clipboardManager,
                    clipLabel = "Transaction Hash",
                )

                // From / To addresses
                if (!isMiningReward && tx.counterparty != null) {
                    val addressLabel = if (isReceived) "From" else "To"
                    DetailRowWithCopy(
                        label = addressLabel,
                        value = tx.counterparty,
                        displayValue = truncateAddress(tx.counterparty),
                        clipboardManager = clipboardManager,
                        clipLabel = "$addressLabel Address",
                    )
                }

                Divider()

                // Timestamp
                DetailRow(
                    label = "Time",
                    value = dateFormat.format(Date(tx.timestampMillis)),
                )

                // Status
                DetailRow(
                    label = "Status",
                    value = tx.status.replaceFirstChar { it.uppercase() },
                )
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text("Close")
            }
        },
    )
}

/**
 * A simple label + value row for the transaction detail dialog.
 */
@Composable
private fun DetailRow(
    label: String,
    value: String,
    valueColor: Color = MaterialTheme.colorScheme.onSurface,
) {
    Column {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )
        Spacer(modifier = Modifier.height(2.dp))
        Text(
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.Medium,
            color = valueColor,
        )
    }
}

/**
 * A label + truncated value row with a copy-to-clipboard button.
 *
 * WHY: Transaction hashes and addresses are too long to display in full
 * within a dialog, but users need the full value for verification. The
 * copy button lets them grab the complete string.
 */
@Composable
private fun DetailRowWithCopy(
    label: String,
    value: String,
    displayValue: String,
    clipboardManager: androidx.compose.ui.platform.ClipboardManager,
    clipLabel: String,
) {
    Column {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
        )
        Spacer(modifier = Modifier.height(2.dp))
        Row(
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = displayValue,
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.weight(1f),
            )
            IconButton(
                onClick = {
                    clipboardManager.setText(AnnotatedString(value))
                },
                modifier = Modifier.size(28.dp),
            ) {
                Icon(
                    Icons.Default.ContentCopy,
                    contentDescription = "Copy $clipLabel",
                    modifier = Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
            }
        }
    }
}

@Composable
private fun StatusChip(status: String) {
    val chipColor = when (status) {
        "confirmed" -> MaterialTheme.colorScheme.primary
        "pending" -> AmberGold
        "failed" -> MaterialTheme.colorScheme.error
        else -> MaterialTheme.colorScheme.outline
    }
    SuggestionChip(
        onClick = {},
        label = {
            Text(
                text = status.replaceFirstChar { it.uppercase() },
                style = MaterialTheme.typography.labelSmall,
            )
        },
    )
}

@Composable
private fun EmptyTransactionState() {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 48.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = "No transactions yet",
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
            textAlign = TextAlign.Center,
        )
    }
}

// ============================================================================
// Backup Reminder Banner
// ============================================================================

/**
 * Amber-colored banner prompting the user to back up their seed phrase.
 *
 * WHY: If the user uninstalls the app without exporting their seed phrase,
 * their GRAT is permanently lost (unless they use PoL behavioral recovery
 * on a new device, which takes 7-14 days). This banner provides a clear,
 * visible nudge to export. Styled similarly to the battery optimization
 * warning card in SettingsScreen for visual consistency.
 */
@Composable
private fun BackupReminderBanner(onClick: () -> Unit) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
        colors = CardDefaults.cardColors(
            containerColor = Color(0xFFFFF3E0), // Light amber background
        ),
        elevation = CardDefaults.cardElevation(defaultElevation = 2.dp),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                Icons.Default.Warning,
                contentDescription = null,
                tint = Color(0xFFE65100), // Deep orange
                modifier = Modifier.size(24.dp),
            )
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = "Back up your wallet!",
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.Bold,
                    color = Color(0xFFE65100),
                )
                Text(
                    text = "If you uninstall the app, your GRAT will be lost. Tap to export seed phrase.",
                    style = MaterialTheme.typography.bodySmall,
                    color = Color(0xFF4E342E),
                )
            }
        }
    }
}

// ============================================================================
// Restore Wallet Dialog (inline from WalletScreen)
// ============================================================================

/**
 * Dialog for restoring a wallet from a hex seed phrase.
 * Used from the WalletScreen's "Restore existing wallet?" link.
 */
@Composable
private fun RestoreWalletInlineDialog(
    error: String?,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var seedHex by remember { mutableStateOf("") }
    val clipboardManager = LocalClipboardManager.current

    AlertDialog(
        onDismissRequest = onDismiss,
        icon = {
            Icon(
                Icons.Default.Key,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
        },
        title = { Text("Restore Wallet") },
        text = {
            Column {
                Text(
                    text = "Paste your hex-encoded seed phrase below. This will replace your current wallet.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                )
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "Warning: Your current wallet will be overwritten.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                Spacer(modifier = Modifier.height(12.dp))
                OutlinedTextField(
                    value = seedHex,
                    onValueChange = { seedHex = it.trim() },
                    label = { Text("Seed phrase (hex)") },
                    placeholder = { Text("64 hex characters...") },
                    isError = error != null,
                    supportingText = error?.let { { Text(it) } },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    trailingIcon = {
                        IconButton(onClick = {
                            clipboardManager.getText()?.let { pasted ->
                                seedHex = pasted.text.trim()
                            }
                        }) {
                            Icon(
                                Icons.Default.ContentCopy,
                                contentDescription = "Paste from clipboard",
                            )
                        }
                    },
                )
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    if (seedHex.length == 64 && seedHex.matches(Regex("[0-9a-fA-F]+"))) {
                        onConfirm(seedHex.lowercase())
                    }
                },
                enabled = seedHex.length == 64 && seedHex.matches(Regex("[0-9a-fA-F]+")),
            ) {
                Text("Restore")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

// ============================================================================
// Send Dialog
// ============================================================================

@Composable
private fun SendDialog(
    initialAddress: String? = null,
    balanceLux: Long = 0,
    onDismiss: () -> Unit,
    onSend: (address: String, amountLux: Long) -> Unit,
    onScanClick: () -> Unit = {},
) {
    var toAddress by remember(initialAddress) { mutableStateOf(initialAddress ?: "") }
    var amountText by remember { mutableStateOf("") }
    var addressError by remember { mutableStateOf<String?>(null) }
    var amountError by remember { mutableStateOf<String?>(null) }

    // WHY: Load contacts once when the dialog opens. Contacts are stored
    // locally and the list is small (max 100), so no need for async loading.
    val contacts = remember { AddressBook.getContacts() }
    var showAddContactDialog by remember { mutableStateOf(false) }

    val clipboardManager = LocalClipboardManager.current

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Send GRAT") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                OutlinedTextField(
                    value = toAddress,
                    onValueChange = {
                        toAddress = it
                        addressError = null
                    },
                    label = { Text("Recipient address") },
                    placeholder = { Text("grat:...") },
                    isError = addressError != null,
                    supportingText = addressError?.let { { Text(it) } },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    trailingIcon = {
                        Row {
                            // WHY: Scan button opens camera to read the recipient's
                            // QR code. This is the primary UX for phone-to-phone
                            // transfers — much faster than copy/paste.
                            IconButton(onClick = onScanClick) {
                                Icon(
                                    Icons.Default.QrCodeScanner,
                                    contentDescription = "Scan QR code",
                                    modifier = Modifier.size(20.dp),
                                )
                            }
                            // Paste from clipboard as fallback
                            IconButton(onClick = {
                                clipboardManager.getText()?.let { pasted ->
                                    toAddress = pasted.text
                                    addressError = null
                                }
                            }) {
                                Icon(
                                    Icons.Default.ContentCopy,
                                    contentDescription = "Paste from clipboard",
                                    modifier = Modifier.size(20.dp),
                                )
                            }
                        }
                    },
                )

                // Contacts section — shows saved contacts as tappable chips
                if (contacts.isNotEmpty()) {
                    Text(
                        text = "Contacts",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                    // WHY: Wrapping contacts in a Column of Rows keeps the dialog
                    // scrollable without needing ExperimentalLayoutApi FlowRow.
                    // Max 3 chips per row to fit the dialog width comfortably.
                    contacts.chunked(2).forEach { rowContacts ->
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            rowContacts.forEach { contact ->
                                SuggestionChip(
                                    onClick = {
                                        toAddress = contact.address
                                        addressError = null
                                    },
                                    label = {
                                        Column {
                                            Text(
                                                text = contact.name,
                                                style = MaterialTheme.typography.labelMedium,
                                                maxLines = 1,
                                                overflow = TextOverflow.Ellipsis,
                                            )
                                            Text(
                                                text = "${contact.address.take(10)}...${contact.address.takeLast(4)}",
                                                style = MaterialTheme.typography.labelSmall,
                                                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                                                maxLines = 1,
                                            )
                                        }
                                    },
                                )
                            }
                        }
                    }
                }

                // Add to contacts — shown when the user has typed a valid-looking address
                // that is not already saved
                if (toAddress.matches(Regex("grat:[0-9a-f]{64}")) &&
                    contacts.none { it.address == toAddress }
                ) {
                    TextButton(
                        onClick = { showAddContactDialog = true },
                        modifier = Modifier.padding(0.dp),
                        contentPadding = PaddingValues(horizontal = 4.dp, vertical = 0.dp),
                    ) {
                        Text(
                            text = "+ Add to contacts",
                            style = MaterialTheme.typography.labelSmall,
                        )
                    }
                }

                OutlinedTextField(
                    value = amountText,
                    onValueChange = {
                        amountText = it
                        amountError = null
                    },
                    label = { Text("Amount (GRAT)") },
                    placeholder = { Text("0.00") },
                    isError = amountError != null,
                    supportingText = amountError?.let { { Text(it) } },
                    singleLine = true,
                    trailingIcon = {
                        if (balanceLux > 0) {
                            TextButton(
                                onClick = {
                                    // WHY: Subtract 1000 Lux (0.001 GRAT) for the tx fee
                                    // so the transaction doesn't fail on insufficient balance.
                                    val maxLux = (balanceLux - 1000).coerceAtLeast(0)
                                    val maxGrat = maxLux.toDouble() / 1_000_000.0
                                    amountText = if (maxGrat == maxGrat.toLong().toDouble()) {
                                        maxGrat.toLong().toString()
                                    } else {
                                        String.format("%.6f", maxGrat).trimEnd('0').trimEnd('.')
                                    }
                                    amountError = null
                                },
                            ) {
                                Text(
                                    "MAX",
                                    fontWeight = FontWeight.Bold,
                                    color = MaterialTheme.colorScheme.primary,
                                )
                            }
                        }
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
                if (balanceLux > 0) {
                    Text(
                        text = "Available: ${formatGrat(balanceLux)} GRAT",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    // Validate address — must be "grat:" followed by exactly 64 lowercase hex chars.
                    // WHY: Gratia addresses are derived from Ed25519 public key hashes and are
                    // always lowercase. Accepting uppercase would allow ambiguous representations
                    // of the same address, which could cause user confusion and checksum issues.
                    if (!toAddress.matches(Regex("grat:[0-9a-f]{64}"))) {
                        addressError = "Invalid address format"
                        return@Button
                    }
                    // Validate amount
                    val gratAmount = amountText.toDoubleOrNull()
                    if (gratAmount == null || gratAmount <= 0) {
                        amountError = "Enter a valid amount"
                        return@Button
                    }
                    // Convert GRAT to Lux (1 GRAT = 1,000,000 Lux)
                    val lux = (gratAmount * 1_000_000).toLong()
                    onSend(toAddress, lux)
                },
            ) {
                Text("Send")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )

    // Add-to-contacts dialog — lets the user save the entered address with a nickname
    if (showAddContactDialog) {
        AddContactDialog(
            address = toAddress,
            onSave = { contactName ->
                AddressBook.addContact(contactName, toAddress)
                showAddContactDialog = false
            },
            onDismiss = { showAddContactDialog = false },
        )
    }
}

// ============================================================================
// Add Contact Dialog
// ============================================================================

@Composable
private fun AddContactDialog(
    address: String,
    onSave: (name: String) -> Unit,
    onDismiss: () -> Unit,
) {
    var contactName by remember { mutableStateOf("") }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Save Contact") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    text = "${address.take(10)}...${address.takeLast(4)}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                )
                OutlinedTextField(
                    value = contactName,
                    onValueChange = { contactName = it },
                    label = { Text("Contact name") },
                    placeholder = { Text("e.g. Alice, Mom") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            Button(
                onClick = { onSave(contactName.trim()) },
                enabled = contactName.isNotBlank(),
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

// ============================================================================
// Receive Dialog
// ============================================================================

/**
 * Generate a QR code bitmap from a string using ZXing.
 *
 * WHY: Wallet addresses are 69 characters (grat:<64 hex>), impractical to
 * type manually. QR codes enable phone-to-phone transfers by scanning.
 */
private fun generateQrBitmap(content: String, size: Int = 512): Bitmap {
    val writer = QRCodeWriter()
    val bitMatrix = writer.encode(content, BarcodeFormat.QR_CODE, size, size)
    val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
    for (x in 0 until size) {
        for (y in 0 until size) {
            bitmap.setPixel(x, y, if (bitMatrix[x, y]) {
                android.graphics.Color.BLACK
            } else {
                android.graphics.Color.WHITE
            })
        }
    }
    return bitmap
}

@Composable
private fun ReceiveDialog(
    address: String,
    onDismiss: () -> Unit,
) {
    val clipboardManager = LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }

    // WHY: Generate QR bitmap once and cache via remember. Re-generates
    // only if the address changes (which it won't during dialog lifetime).
    val qrBitmap = remember(address) { generateQrBitmap(address) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Receive GRAT") },
        text = {
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                // QR code
                Card(
                    modifier = Modifier.size(220.dp),
                    colors = CardDefaults.cardColors(
                        containerColor = Color.White,
                    ),
                ) {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(12.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Image(
                            bitmap = qrBitmap.asImageBitmap(),
                            contentDescription = "Wallet QR Code",
                            modifier = Modifier.fillMaxSize(),
                        )
                    }
                }

                // Address display
                Divider()
                Text(
                    text = address,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.fillMaxWidth(),
                )

                // Copy button
                OutlinedButton(
                    onClick = {
                        clipboardManager.setText(AnnotatedString(address))
                        copied = true
                    },
                ) {
                    Icon(
                        Icons.Default.ContentCopy,
                        contentDescription = null,
                        modifier = Modifier.size(16.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(if (copied) "Copied!" else "Copy Address")
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text("Done")
            }
        },
    )
}

// ============================================================================
// Offline Banner
// ============================================================================

/**
 * Dismissible amber-colored banner shown when the network layer is not running.
 *
 * WHY: Users need to know their wallet is offline so they understand that
 * sends will not propagate and balance may be stale. Amber (not red) because
 * the wallet is still usable for viewing — it's a warning, not an error.
 */
@Composable
fun OfflineBanner() {
    var dismissed by remember { mutableStateOf(false) }
    if (dismissed) return

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = AmberGold.copy(alpha = 0.15f),
        ),
    ) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                Icons.Default.Warning,
                contentDescription = "Offline warning",
                tint = AmberGold,
                modifier = Modifier.size(20.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            Text(
                text = "You're offline \u2014 some features may be unavailable",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.weight(1f),
            )
            IconButton(
                onClick = { dismissed = true },
                modifier = Modifier.size(28.dp),
            ) {
                Icon(
                    Icons.Default.Close,
                    contentDescription = "Dismiss",
                    modifier = Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                )
            }
        }
    }
}
