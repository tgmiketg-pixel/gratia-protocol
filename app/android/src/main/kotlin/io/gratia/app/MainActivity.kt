package io.gratia.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccountBalance
import androidx.compose.material.icons.filled.BoltCircle
import androidx.compose.material.icons.filled.CellTower
import androidx.compose.material.icons.filled.HowToVote
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.AccountBalance
import androidx.compose.material.icons.outlined.BoltCircle
import androidx.compose.material.icons.outlined.CellTower
import androidx.compose.material.icons.outlined.HowToVote
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.navigation.NavDestination.Companion.hierarchy
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import io.gratia.app.ui.theme.GratiaTheme

/**
 * Main activity — the single Activity for the entire app.
 *
 * Uses Jetpack Compose for the UI with a bottom navigation bar routing
 * between the four core screens: Wallet, Mining, Governance, Settings.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            GratiaTheme {
                GratiaApp()
            }
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
        selectedIcon = Icons.Filled.BoltCircle,
        unselectedIcon = Icons.Outlined.BoltCircle,
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

@Composable
fun GratiaApp() {
    val navController = rememberNavController()
    val navBackStackEntry by navController.currentBackStackEntryAsState()
    val currentDestination = navBackStackEntry?.destination

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        bottomBar = {
            NavigationBar {
                bottomNavTabs.forEach { tab ->
                    val selected = currentDestination?.hierarchy?.any {
                        it.route == tab.route
                    } == true

                    NavigationBarItem(
                        selected = selected,
                        onClick = {
                            navController.navigate(tab.route) {
                                // Pop up to the start destination to avoid building
                                // up a large back stack when switching tabs.
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
                            Text(text = stringResource(tab.labelResId))
                        },
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
                // TODO: WalletScreen() — implemented by UI agent
                ScreenPlaceholder(name = stringResource(R.string.tab_wallet))
            }
            composable(GratiaRoutes.MINING) {
                // TODO: MiningScreen() — implemented by UI agent
                ScreenPlaceholder(name = stringResource(R.string.tab_mining))
            }
            composable(GratiaRoutes.NETWORK) {
                io.gratia.app.ui.NetworkScreen()
            }
            composable(GratiaRoutes.GOVERNANCE) {
                // TODO: GovernanceScreen() — implemented by UI agent
                ScreenPlaceholder(name = stringResource(R.string.tab_governance))
            }
            composable(GratiaRoutes.SETTINGS) {
                // TODO: SettingsScreen() — implemented by UI agent
                ScreenPlaceholder(name = stringResource(R.string.tab_settings))
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
