import Foundation
import NetworkExtension
import SystemConfiguration.CaptiveNetwork
import CryptoKit

// ============================================================================
// WifiManager — Wi-Fi network detection for PoL attestation
//
// Mirrors Android's WifiManager.kt:
// - Collects Wi-Fi network BSSID hashes (privacy-preserving)
// - Posts WifiScan sensor events
//
// WHY NEHotspotHelper vs CNCopyCurrentNetworkInfo:
// - CNCopyCurrentNetworkInfo is deprecated in iOS 14+ and requires
//   entitlements that are difficult to obtain.
// - NEHotspotHelper requires the com.apple.developer.networking.HotspotHelper
//   entitlement (granted by Apple on request).
// - As a fallback, we use NEHotspotNetwork.fetchCurrent() (iOS 14+) which
//   works with the standard Access WiFi Information entitlement.
//
// PRIVACY: Raw BSSIDs are never stored or transmitted. They are immediately
// hashed on-device and only the hash is forwarded.
// ============================================================================

final class WifiManager: ObservableObject {

    private let logger = GratiaLogger(tag: "GratiaWifiManager")

    private var isRunning = false
    private var scanTimer: Timer?

    /// Callback for PoL sensor events.
    var onWifiScan: (([UInt64]) -> Void)?

    // WHY: 15-minute interval matches the GPS scan interval. Wi-Fi network
    // identity changes less frequently than Bluetooth environments, so fewer
    // scans are needed. One detected network per day satisfies the PoL
    // "connected to at least one Wi-Fi network" parameter.
    private static let scanIntervalSeconds: TimeInterval = 15 * 60 // 15 minutes

    /// Start periodic Wi-Fi network scanning.
    func start() {
        guard !isRunning else { return }

        isRunning = true
        performScan()
        schedulePeriodic()
        logger.info("Wi-Fi scanning started (interval=\(Int(Self.scanIntervalSeconds / 60))min)")
    }

    /// Stop scanning.
    func stop() {
        guard isRunning else { return }

        scanTimer?.invalidate()
        scanTimer = nil
        isRunning = false
        logger.info("Wi-Fi scanning stopped")
    }

    /// Whether this manager is actively scanning.
    var isActive: Bool { isRunning }

    // MARK: - Private

    private func schedulePeriodic() {
        scanTimer?.invalidate()
        scanTimer = Timer.scheduledTimer(withTimeInterval: Self.scanIntervalSeconds, repeats: true) { [weak self] _ in
            self?.performScan()
        }
    }

    private func performScan() {
        // WHY: iOS 14+ API for fetching the current Wi-Fi network.
        // This is the most reliable method that works with standard entitlements.
        if #available(iOS 14.0, *) {
            NEHotspotNetwork.fetchCurrent { [weak self] network in
                guard let self = self else { return }

                if let network = network {
                    let bssidHash = self.hashBssid(network.bssid)
                    self.logger.debug("Wi-Fi scan: SSID=\(network.ssid), BSSID hash captured")
                    self.onWifiScan?([bssidHash])
                } else {
                    // WHY: nil means either Wi-Fi is off, the device is not connected
                    // to any network, or the entitlement is missing. This is non-fatal;
                    // the PoL network parameter can also be satisfied by Bluetooth.
                    self.logger.debug("Wi-Fi scan: no current network")
                }
            }
        } else {
            // Fallback for older iOS versions using CNCopyCurrentNetworkInfo
            fetchViaLegacyApi()
        }
    }

    /// Legacy fallback using CNCopyCurrentNetworkInfo (iOS < 14).
    private func fetchViaLegacyApi() {
        guard let interfaces = CNCopySupportedInterfaces() as? [String] else {
            logger.debug("Wi-Fi scan: no supported interfaces")
            return
        }

        var hashes: [UInt64] = []

        for interface in interfaces {
            guard let info = CNCopyCurrentNetworkInfo(interface as CFString) as? [String: Any],
                  let bssid = info[kCNNetworkInfoKeyBSSID as String] as? String else {
                continue
            }
            hashes.append(hashBssid(bssid))
        }

        if hashes.isEmpty {
            logger.debug("Wi-Fi scan: no networks detected (legacy API)")
        } else {
            logger.debug("Wi-Fi scan: \(hashes.count) network(s) detected")
            onWifiScan?(hashes)
        }
    }

    /// Hash a BSSID string to an opaque 8-byte UInt64.
    ///
    /// PRIVACY: Raw BSSIDs are never stored or sent. The hash is a one-way
    /// transformation for detecting "same network seen again".
    private func hashBssid(_ bssid: String) -> UInt64 {
        let data = Data(bssid.utf8)
        let hash = SHA256.hash(data: data)

        var result: UInt64 = 0
        let bytes = Array(hash)
        for i in 0..<8 {
            result |= UInt64(bytes[i]) << (i * 8)
        }
        return result
    }
}
