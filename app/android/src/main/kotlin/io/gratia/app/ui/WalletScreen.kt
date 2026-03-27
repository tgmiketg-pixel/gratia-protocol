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
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.CallMade
import androidx.compose.material.icons.automirrored.filled.CallReceived
import androidx.compose.material.icons.filled.ContentCopy
import io.gratia.app.GratiaLogo
import androidx.compose.material.icons.filled.QrCode
import androidx.compose.material.icons.filled.QrCodeScanner
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
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val context = LocalContext.current

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

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = { GratiaLogo(modifier = Modifier.padding(start = 12.dp)) },
                title = { Text("Wallet") },
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
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            ) {
                WalletContent(
                    state = state,
                    onSendClick = { viewModel.showSendDialog() },
                    onReceiveClick = { viewModel.showReceiveDialog() },
                )
            }
        }

        // Send dialog
        if (state.showSendDialog) {
            SendDialog(
                initialAddress = scannedAddress,
                onDismiss = {
                    viewModel.hideSendDialog()
                    scannedAddress = null
                },
                onSend = { address, amount ->
                    viewModel.sendTransfer(address, amount)
                    scannedAddress = null
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
    onSendClick: () -> Unit,
    onReceiveClick: () -> Unit,
) {
    val wallet = state.walletInfo ?: return

    LazyColumn(
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        // Balance card
        item {
            BalanceCard(
                wallet = wallet,
                onSendClick = onSendClick,
                onReceiveClick = onReceiveClick,
            )
        }

        // Transaction history header
        item {
            Text(
                text = "Recent Transactions",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
        }

        // Transaction list or empty state
        if (state.transactions.isEmpty()) {
            item {
                EmptyTransactionState()
            }
        } else {
            items(state.transactions, key = { it.hashHex }) { tx ->
                TransactionRow(tx)
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
private fun TransactionRow(tx: TransactionInfo) {
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
        modifier = Modifier.fillMaxWidth(),
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
// Send Dialog
// ============================================================================

@Composable
private fun SendDialog(
    initialAddress: String? = null,
    onDismiss: () -> Unit,
    onSend: (address: String, amountLux: Long) -> Unit,
    onScanClick: () -> Unit = {},
) {
    var toAddress by remember(initialAddress) { mutableStateOf(initialAddress ?: "") }
    var amountText by remember { mutableStateOf("") }
    var addressError by remember { mutableStateOf<String?>(null) }
    var amountError by remember { mutableStateOf<String?>(null) }

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
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    // Validate address — must be "grat:" followed by exactly 64 hex chars
                    if (!toAddress.matches(Regex("grat:[0-9a-fA-F]{64}"))) {
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
