import SwiftUI

// ============================================================================
// Gratia iOS App — SwiftUI Entry Point
//
// Five-tab navigation mirroring the Android app exactly:
// Wallet, Mining, Network, Governance, Settings
//
// Color scheme: DeepNavy, AmberGold, WarmWhite (from Brand Identity Guide v1.0)
// ============================================================================

// MARK: - Brand Colors

/// Gratia brand colors — derived from Brand Identity Guide v1.0.
/// Every color traces back to the logo SVG. No off-brand colors allowed.
extension Color {
    // Primary
    static let deepNavy = Color(red: 0x1A / 255.0, green: 0x27 / 255.0, blue: 0x44 / 255.0)
    static let amberGold = Color(red: 0xF5 / 255.0, green: 0xA6 / 255.0, blue: 0x23 / 255.0)

    // Secondary: The Instrument Palette
    static let darkGoldenrod = Color(red: 0xB8 / 255.0, green: 0x86 / 255.0, blue: 0x0B / 255.0)
    static let darkAmber = Color(red: 0xD4 / 255.0, green: 0x89 / 255.0, blue: 0x0F / 255.0)
    static let golden = Color(red: 0xE8 / 255.0, green: 0xA0 / 255.0, blue: 0x20 / 255.0)
    static let agedGold = Color(red: 0x8B / 255.0, green: 0x69 / 255.0, blue: 0x14 / 255.0)

    // Extended Palette
    static let midnight = Color(red: 0x0D / 255.0, green: 0x15 / 255.0, blue: 0x27 / 255.0)
    static let charcoalNavy = Color(red: 0x2A / 255.0, green: 0x3A / 255.0, blue: 0x5C / 255.0)
    static let warmWhite = Color(red: 0xFA / 255.0, green: 0xF5 / 255.0, blue: 0xEB / 255.0)
    static let offWhite = Color(red: 0xF0 / 255.0, green: 0xE8 / 255.0, blue: 0xD8 / 255.0)
    static let lightGold = Color(red: 0xFD / 255.0, green: 0xD8 / 255.0, blue: 0x88 / 255.0)
    static let paleAmber = Color(red: 0xFE / 255.0, green: 0xF3 / 255.0, blue: 0xD5 / 255.0)

    // Status
    static let signalGreen = Color(red: 0x2E / 255.0, green: 0xCC / 255.0, blue: 0x71 / 255.0)
    static let alertRed = Color(red: 0xE7 / 255.0, green: 0x4C / 255.0, blue: 0x3C / 255.0)
}

// MARK: - Tab Definition

/// Navigation tab identifiers.
enum GratiaTab: String, CaseIterable {
    case wallet
    case mining
    case network
    case governance
    case settings

    var label: String {
        switch self {
        case .wallet: return "Wallet"
        case .mining: return "Mining"
        case .network: return "Network"
        case .governance: return "Governance"
        case .settings: return "Settings"
        }
    }

    var systemImage: String {
        switch self {
        case .wallet: return "building.columns"
        case .mining: return "bolt.fill"
        case .network: return "antenna.radiowaves.left.and.right"
        case .governance: return "hand.raised.fill"
        case .settings: return "gearshape"
        }
    }
}

// MARK: - App Entry Point

@main
struct GratiaApp: App {
    @State private var selectedTab: GratiaTab = .wallet

    init() {
        configureAppearance()
        initializeRustCore()
    }

    var body: some Scene {
        WindowGroup {
            TabView(selection: $selectedTab) {
                WalletView()
                    .tabItem {
                        Label(GratiaTab.wallet.label, systemImage: GratiaTab.wallet.systemImage)
                    }
                    .tag(GratiaTab.wallet)

                MiningView()
                    .tabItem {
                        Label(GratiaTab.mining.label, systemImage: GratiaTab.mining.systemImage)
                    }
                    .tag(GratiaTab.mining)

                NetworkView()
                    .tabItem {
                        Label(GratiaTab.network.label, systemImage: GratiaTab.network.systemImage)
                    }
                    .tag(GratiaTab.network)

                GovernanceView()
                    .tabItem {
                        Label(GratiaTab.governance.label, systemImage: GratiaTab.governance.systemImage)
                    }
                    .tag(GratiaTab.governance)

                SettingsView()
                    .tabItem {
                        Label(GratiaTab.settings.label, systemImage: GratiaTab.settings.systemImage)
                    }
                    .tag(GratiaTab.settings)
            }
            .tint(.amberGold)
        }
    }

    /// Configure UIKit appearance proxies for the Gratia brand.
    ///
    /// WHY: SwiftUI TabView still uses UIKit's UITabBar under the hood.
    /// We configure appearance here to match the DeepNavy/AmberGold brand.
    private func configureAppearance() {
        let tabBarAppearance = UITabBarAppearance()
        tabBarAppearance.configureWithOpaqueBackground()
        tabBarAppearance.backgroundColor = UIColor(Color.deepNavy)

        // Selected tab: AmberGold
        tabBarAppearance.stackedLayoutAppearance.selected.iconColor = UIColor(Color.amberGold)
        tabBarAppearance.stackedLayoutAppearance.selected.titleTextAttributes = [
            .foregroundColor: UIColor(Color.amberGold)
        ]

        // Unselected tab: WarmWhite at 60% opacity
        let unselectedColor = UIColor(Color.warmWhite).withAlphaComponent(0.6)
        tabBarAppearance.stackedLayoutAppearance.normal.iconColor = unselectedColor
        tabBarAppearance.stackedLayoutAppearance.normal.titleTextAttributes = [
            .foregroundColor: unselectedColor
        ]

        UITabBar.appearance().standardAppearance = tabBarAppearance
        UITabBar.appearance().scrollEdgeAppearance = tabBarAppearance

        // Navigation bar appearance
        let navBarAppearance = UINavigationBarAppearance()
        navBarAppearance.configureWithOpaqueBackground()
        navBarAppearance.backgroundColor = UIColor.systemBackground
        UINavigationBar.appearance().standardAppearance = navBarAppearance
        UINavigationBar.appearance().scrollEdgeAppearance = navBarAppearance
    }

    /// Initialize the shared Rust core via UniFFI bridge.
    ///
    /// WHY: The Rust core must be initialized before any UI or sensor code
    /// calls into it. We use the app's Documents directory as the data store
    /// (same pattern as Android's filesDir).
    private func initializeRustCore() {
        let documentsPath = FileManager.default.urls(
            for: .documentDirectory, in: .userDomainMask
        ).first!.path

        Task { @MainActor in
            do {
                try GratiaCoreManager.shared.initialize(dataDir: documentsPath)
            } catch {
                print("[GratiaApp] Failed to initialize Rust core: \(error.localizedDescription)")
            }
        }
    }
}

// MARK: - Utility Functions

/// Format a Lux amount as a human-readable GRAT string.
/// 1 GRAT = 1,000,000 Lux.
func formatGrat(_ lux: Int64) -> String {
    let whole = lux / 1_000_000
    let fractional = abs(lux % 1_000_000)
    if fractional == 0 {
        return "\(whole)"
    }
    // WHY: Trim trailing zeros for cleaner display (e.g., "1.5" instead of "1.500000")
    let fractStr = String(format: "%06d", fractional)
    let trimmed = fractStr.replacingOccurrences(of: "0+$", with: "", options: .regularExpression)
    return "\(whole).\(trimmed)"
}

/// Truncate a Gratia address for display (e.g., "grat:abc123...def456").
func truncateAddress(_ address: String) -> String {
    guard address.count > 16 else { return address }
    let prefix = address.prefix(12)
    let suffix = address.suffix(6)
    return "\(prefix)...\(suffix)"
}
