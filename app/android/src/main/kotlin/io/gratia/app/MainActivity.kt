package io.gratia.app

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.nfc.NfcAdapter
import android.nfc.Tag
import android.nfc.tech.IsoDep
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import io.gratia.app.service.ProofOfLifeService
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccountBalance
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.CellTower
import androidx.compose.material.icons.filled.HowToVote
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.AccountBalance
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.CellTower
import androidx.compose.material.icons.outlined.HowToVote
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.navigation.NavDestination.Companion.hierarchy
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import io.gratia.app.ui.theme.AmberGold
import io.gratia.app.ui.theme.DeepNavy
import io.gratia.app.ui.theme.GratiaTheme
import io.gratia.app.ui.theme.WarmWhite

/**
 * Main activity — the single Activity for the entire app.
 *
 * Uses Jetpack Compose for the UI with a bottom navigation bar routing
 * between the four core screens: Wallet, Mining, Governance, Settings.
 */
class MainActivity : ComponentActivity(), NfcAdapter.ReaderCallback {

    companion object {
        private const val TAG = "GratiaMainActivity"

        // SharedPreferences key to track whether we've already prompted the user
        // for battery optimization exemption. We only ask once to avoid nagging.
        private const val PREFS_NAME = "gratia_prefs"
        private const val KEY_BATTERY_OPT_ASKED = "battery_optimization_asked"

        // WHY: SELECT APDU sends our custom AID to the other phone's HCE service.
        // The receiver's NfcTransactService matches this AID and responds with
        // its wallet address.
        private val SELECT_APDU = byteArrayOf(
            0x00.toByte(), // CLA
            0xA4.toByte(), // INS (SELECT)
            0x04.toByte(), // P1 (select by name)
            0x00.toByte(), // P2
            0x07.toByte(), // Lc (AID length = 7 bytes)
            0xF0.toByte(), 0x47, 0x52, 0x41, 0x54, 0x49, 0x41, // AID: F0475241544941
            0x00.toByte()  // Le (accept any response length)
        )

        /**
         * NFC-scanned wallet address flow.
         *
         * WHY: Static StateFlow so WalletScreen can observe it from any composable
         * context without needing a reference to the Activity. The flow emits null
         * when no address has been scanned, and a wallet address string after a
         * successful NFC tap. The consumer resets it to null after handling.
         */
        private val _nfcScannedAddress = MutableStateFlow<String?>(null)
        val nfcScannedAddress: StateFlow<String?> = _nfcScannedAddress.asStateFlow()

        /** Reset the scanned address after it has been consumed by the UI. */
        fun clearNfcScannedAddress() {
            _nfcScannedAddress.value = null
        }
    }

    private var nfcAdapter: NfcAdapter? = null

    /** Permission launcher — requests location then starts PoL service. */
    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { permissions ->
        val locationGranted = permissions[Manifest.permission.ACCESS_FINE_LOCATION] == true
                || permissions[Manifest.permission.ACCESS_COARSE_LOCATION] == true
        if (locationGranted) {
            Log.i(TAG, "Location permission granted — starting PoL service")
            startPolService()
        } else {
            Log.w(TAG, "Location permission denied — PoL service will not start")
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        nfcAdapter = NfcAdapter.getDefaultAdapter(this)

        setContent {
            GratiaTheme {
                GratiaApp()
            }
        }
        requestPermissionsAndStartPoL()
        requestBatteryOptimizationExemption()
    }

    override fun onResume() {
        super.onResume()
        enableNfcReaderMode()
    }

    override fun onPause() {
        super.onPause()
        disableNfcReaderMode()
    }

    // ========================================================================
    // NFC Reader Mode
    // ========================================================================

    /**
     * Enable NFC reader mode to detect other phones running the Gratia HCE service.
     *
     * WHY: Reader mode with FLAG_READER_SKIP_NDEF_CHECK avoids the Android
     * default NDEF tag-dispatch system, which would show a chooser dialog.
     * Instead we get raw IsoDep access to send our SELECT APDU directly.
     */
    private fun enableNfcReaderMode() {
        val adapter = nfcAdapter ?: return

        // WHY: NFC_A covers most Android HCE devices. NFC_B covers some
        // international chip variants. SKIP_NDEF_CHECK prevents Android from
        // trying to read NDEF records (we use raw ISO-DEP APDU instead).
        val flags = NfcAdapter.FLAG_READER_NFC_A or
                NfcAdapter.FLAG_READER_NFC_B or
                NfcAdapter.FLAG_READER_SKIP_NDEF_CHECK

        adapter.enableReaderMode(this, this, flags, null)
        Log.d(TAG, "NFC reader mode enabled")
    }

    private fun disableNfcReaderMode() {
        val adapter = nfcAdapter ?: return
        adapter.disableReaderMode(this)
        Log.d(TAG, "NFC reader mode disabled")
    }

    /**
     * Called when an NFC tag (or HCE device) is discovered in reader mode.
     *
     * Sends the Gratia SELECT APDU and reads the wallet address response.
     * Runs on a binder thread — NOT the main thread.
     */
    override fun onTagDiscovered(tag: Tag?) {
        if (tag == null) return

        val isoDep = IsoDep.get(tag)
        if (isoDep == null) {
            Log.w(TAG, "NFC tag does not support IsoDep — not a Gratia device")
            return
        }

        try {
            isoDep.connect()

            // WHY: 2-second timeout is generous for a local HCE response that
            // should return in <100ms. Protects against hanging if the other
            // phone's app is in a bad state.
            isoDep.timeout = 2000

            val response = isoDep.transceive(SELECT_APDU)

            // WHY: Minimum valid response is 2 bytes (status word only, no payload).
            // A successful response has payload bytes + 0x9000 status.
            if (response.size < 2) {
                Log.w(TAG, "NFC response too short: ${response.size} bytes")
                return
            }

            // Check status word (last 2 bytes)
            val sw1 = response[response.size - 2]
            val sw2 = response[response.size - 1]

            if (sw1 == 0x90.toByte() && sw2 == 0x00.toByte()) {
                // Strip status word to get the address payload
                val addressBytes = response.copyOfRange(0, response.size - 2)
                val address = String(addressBytes, Charsets.UTF_8)

                // WHY: Basic validation — Gratia addresses start with "grat:"
                // and are 69 chars (grat: + 64 hex). Reject garbage data.
                if (address.startsWith("grat:") && address.length >= 10) {
                    Log.i(TAG, "NFC tap received wallet address: ${address.take(12)}...")
                    _nfcScannedAddress.value = address
                } else {
                    Log.w(TAG, "NFC response is not a valid Gratia address: $address")
                }
            } else {
                Log.w(TAG, "NFC HCE returned error status: %02X%02X".format(sw1, sw2))
            }
        } catch (e: Exception) {
            Log.e(TAG, "NFC communication failed: ${e.message}", e)
        } finally {
            try {
                isoDep.close()
            } catch (_: Exception) {
                // Ignore close errors
            }
        }
    }

    /**
     * Request runtime permissions needed for the PoL foreground service,
     * then start the service once granted.
     */
    private fun requestPermissionsAndStartPoL() {
        val needed = mutableListOf<String>()

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_FINE_LOCATION)
            != PackageManager.PERMISSION_GRANTED
        ) {
            needed.add(Manifest.permission.ACCESS_FINE_LOCATION)
        }

        // Android 12+ Bluetooth permissions
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            if (ContextCompat.checkSelfPermission(this, Manifest.permission.BLUETOOTH_SCAN)
                != PackageManager.PERMISSION_GRANTED
            ) {
                needed.add(Manifest.permission.BLUETOOTH_SCAN)
                needed.add(Manifest.permission.BLUETOOTH_CONNECT)
            }
        }

        // Android 13+ notification permission
        // WHY: Without this, foreground service notifications are silently
        // blocked on Android 13+. The user never sees mining/PoL status.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED
            ) {
                needed.add(Manifest.permission.POST_NOTIFICATIONS)
            }
        }

        if (needed.isEmpty()) {
            // All permissions already granted
            startPolService()
        } else {
            permissionLauncher.launch(needed.toTypedArray())
        }
    }

    /**
     * Request battery optimization exemption so Samsung, Xiaomi, Huawei, and other
     * aggressive OEMs don't kill the ProofOfLifeService in the background.
     *
     * WHY: Many Android OEMs (especially Samsung, Xiaomi, Huawei, Oppo) aggressively
     * kill background services even when they are foreground services with notifications.
     * Without battery optimization exemption, PoL sensor collection silently stops,
     * causing users to fail their daily Proof of Life without knowing why.
     *
     * We only prompt once (tracked via SharedPreferences) to avoid nagging the user.
     * If they decline, the app still works — the service may just get killed more often
     * on aggressive OEMs.
     */
    private fun requestBatteryOptimizationExemption() {
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        val packageName = packageName

        if (powerManager.isIgnoringBatteryOptimizations(packageName)) {
            Log.i(TAG, "Battery optimization already disabled — PoL service is protected")
            return
        }

        val prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        if (prefs.getBoolean(KEY_BATTERY_OPT_ASKED, false)) {
            Log.d(TAG, "Battery optimization prompt already shown once — not asking again")
            return
        }

        try {
            val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                data = Uri.parse("package:$packageName")
            }
            startActivity(intent)
            Log.i(TAG, "Battery optimization exemption requested")
        } catch (e: Exception) {
            Log.w(TAG, "Failed to request battery optimization exemption: ${e.message}", e)
        }

        // WHY: Mark as asked regardless of whether the user granted or denied.
        // We don't want to nag on every app launch.
        prefs.edit().putBoolean(KEY_BATTERY_OPT_ASKED, true).apply()
    }

    private fun startPolService() {
        try {
            val intent = Intent(this, ProofOfLifeService::class.java)
            startForegroundService(intent)
            Log.i(TAG, "PoL service started")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start PoL service: ${e.message}", e)
        }
    }
}

// ============================================================================
// Navigation
// ============================================================================

/**
 * Navigation route constants.
 */
object GratiaRoutes {
    const val WALLET = "wallet"
    const val MINING = "mining"
    const val NETWORK = "network"
    const val GOVERNANCE = "governance"
    const val SETTINGS = "settings"
}

/**
 * Bottom navigation tab definition.
 *
 * @param route Navigation route string.
 * @param labelResId String resource ID for the tab label.
 * @param selectedIcon Icon shown when this tab is active.
 * @param unselectedIcon Icon shown when this tab is inactive.
 */
data class BottomNavTab(
    val route: String,
    val labelResId: Int,
    val selectedIcon: ImageVector,
    val unselectedIcon: ImageVector,
)

/** The four bottom navigation tabs. */
val bottomNavTabs = listOf(
    BottomNavTab(
        route = GratiaRoutes.WALLET,
        labelResId = R.string.tab_wallet,
        selectedIcon = Icons.Filled.AccountBalance,
        unselectedIcon = Icons.Outlined.AccountBalance,
    ),
    BottomNavTab(
        route = GratiaRoutes.MINING,
        labelResId = R.string.tab_mining,
        selectedIcon = Icons.Filled.Bolt,
        unselectedIcon = Icons.Outlined.Bolt,
    ),
    BottomNavTab(
        route = GratiaRoutes.NETWORK,
        labelResId = R.string.tab_network,
        selectedIcon = Icons.Filled.CellTower,
        unselectedIcon = Icons.Outlined.CellTower,
    ),
    BottomNavTab(
        route = GratiaRoutes.GOVERNANCE,
        labelResId = R.string.tab_governance,
        selectedIcon = Icons.Filled.HowToVote,
        unselectedIcon = Icons.Outlined.HowToVote,
    ),
    BottomNavTab(
        route = GratiaRoutes.SETTINGS,
        labelResId = R.string.tab_settings,
        selectedIcon = Icons.Filled.Settings,
        unselectedIcon = Icons.Outlined.Settings,
    ),
)

// ============================================================================
// App Composable
// ============================================================================

/**
 * Gratia full logo — compass ring with phone + heartbeat inside.
 *
 * WHY: Uses ic_gratia_logo.xml which is a simplified version of the full
 * brand SVG (gratia-logo.svg), optimized for small sizes. Keeps the outer
 * compass ring, gold circle, phone silhouette, and ECG heartbeat — the
 * key brand elements that make Gratia instantly recognizable.
 */
@Composable
fun GratiaLogo(modifier: Modifier = Modifier, size: Int = 36) {
    val logoPainter = androidx.compose.ui.res.painterResource(
        id = R.drawable.ic_gratia_logo
    )
    androidx.compose.foundation.Image(
        painter = logoPainter,
        contentDescription = "Gratia",
        modifier = modifier.size(size.dp),
    )
}

@Composable
fun GratiaApp() {
    val navController = rememberNavController()
    val navBackStackEntry by navController.currentBackStackEntryAsState()
    val currentDestination = navBackStackEntry?.destination

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        bottomBar = {
            NavigationBar(
                containerColor = DeepNavy,
                contentColor = WarmWhite,
            ) {
                bottomNavTabs.forEach { tab ->
                    val selected = currentDestination?.hierarchy?.any {
                        it.route == tab.route
                    } == true

                    NavigationBarItem(
                        selected = selected,
                        onClick = {
                            navController.navigate(tab.route) {
                                popUpTo(navController.graph.findStartDestination().id) {
                                    saveState = true
                                }
                                launchSingleTop = true
                                restoreState = true
                            }
                        },
                        icon = {
                            Icon(
                                imageVector = if (selected) tab.selectedIcon else tab.unselectedIcon,
                                contentDescription = stringResource(tab.labelResId),
                            )
                        },
                        label = {
                            Text(
                                text = stringResource(tab.labelResId),
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                fontSize = 11.sp,
                            )
                        },
                        colors = NavigationBarItemDefaults.colors(
                            selectedIconColor = AmberGold,
                            selectedTextColor = AmberGold,
                            unselectedIconColor = WarmWhite.copy(alpha = 0.6f),
                            unselectedTextColor = WarmWhite.copy(alpha = 0.6f),
                            indicatorColor = AmberGold.copy(alpha = 0.12f),
                        ),
                    )
                }
            }
        }
    ) { innerPadding ->
        NavHost(
            navController = navController,
            startDestination = GratiaRoutes.WALLET,
            modifier = Modifier.padding(innerPadding),
        ) {
            composable(GratiaRoutes.WALLET) {
                io.gratia.app.ui.WalletScreen(
                    onNavigateToSettings = {
                        // WHY: Use the same navigation pattern as the bottom bar
                        // so Settings replaces the current tab instead of stacking.
                        // This lets the bottom bar highlight Settings and back
                        // navigation works normally via the tabs.
                        navController.navigate(GratiaRoutes.SETTINGS) {
                            popUpTo(navController.graph.findStartDestination().id) {
                                saveState = true
                            }
                            launchSingleTop = true
                            restoreState = true
                        }
                    },
                )
            }
            composable(GratiaRoutes.MINING) {
                io.gratia.app.ui.MiningScreen()
            }
            composable(GratiaRoutes.NETWORK) {
                io.gratia.app.ui.NetworkScreen()
            }
            composable(GratiaRoutes.GOVERNANCE) {
                io.gratia.app.ui.GovernanceScreen()
            }
            composable(GratiaRoutes.SETTINGS) {
                io.gratia.app.ui.SettingsScreen()
            }
        }
    }
}

/**
 * Temporary placeholder composable for screens not yet implemented.
 * Will be replaced by the actual screen composables from the ui/ package.
 */
@Composable
fun ScreenPlaceholder(name: String) {
    androidx.compose.foundation.layout.Box(
        modifier = Modifier.fillMaxSize(),
        contentAlignment = androidx.compose.ui.Alignment.Center,
    ) {
        Text(
            text = name,
            style = androidx.compose.material3.MaterialTheme.typography.headlineMedium,
        )
    }
}
