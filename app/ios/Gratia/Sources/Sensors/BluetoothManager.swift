import Foundation
import CoreBluetooth
import CryptoKit

// ============================================================================
// BluetoothManager — Core Bluetooth wrapper for PoL peer environment detection
//
// Mirrors Android's BluetoothManager.kt:
// - CBCentralManager for BLE scanning
// - Collects peer device hashes (privacy-preserving)
// - Posts BluetoothScan sensor events
//
// PRIVACY: Raw Bluetooth identifiers are never stored or transmitted.
// They are immediately hashed on-device and only the hash is forwarded.
//
// WHY periodic scanning vs. continuous: Continuous BLE scanning drains
// significant battery. A 10-second scan every 30 minutes is sufficient to
// capture environment snapshots for PoL while consuming negligible power.
// ============================================================================

final class BluetoothManager: NSObject, ObservableObject, CBCentralManagerDelegate {

    private let logger = GratiaLogger(tag: "GratiaBluetooth")

    private var centralManager: CBCentralManager?
    private var isRunning = false
    private var scanTimer: Timer?
    private var stopScanTimer: Timer?

    /// Current scan window's discovered peer hashes.
    private var currentScanPeers = Set<UInt64>()

    /// Callback for PoL sensor events.
    var onBluetoothScan: (([UInt64]) -> Void)?

    // WHY: 30-minute interval provides enough snapshots to detect environment
    // changes throughout the day (up to ~48 snapshots). The PoL requirement
    // is just 2 distinct environments, so even a few successful scans suffice.
    private static let scanIntervalSeconds: TimeInterval = 30 * 60 // 30 minutes

    // WHY: 10-second scan window captures most nearby BLE advertisers.
    // Longer scans increase battery usage without meaningfully improving
    // environment detection for PoL purposes.
    private static let scanDurationSeconds: TimeInterval = 10 // 10 seconds

    override init() {
        super.init()
    }

    /// Start periodic Bluetooth LE scanning.
    ///
    /// The CBCentralManager is initialized lazily to avoid triggering the
    /// Bluetooth permission prompt until the user actually needs it.
    func start() {
        guard !isRunning else { return }

        // WHY: Initializing CBCentralManager immediately triggers the system
        // Bluetooth permission dialog. We defer creation to start() so the
        // dialog appears at a predictable time.
        if centralManager == nil {
            centralManager = CBCentralManager(delegate: self, queue: nil)
        }

        // Actual scanning starts in centralManagerDidUpdateState when BT is powered on.
        isRunning = true
        logger.info("Bluetooth LE scanning requested")
    }

    /// Stop scanning and release resources.
    func stop() {
        guard isRunning else { return }

        scanTimer?.invalidate()
        scanTimer = nil
        stopScanTimer?.invalidate()
        stopScanTimer = nil

        if centralManager?.state == .poweredOn {
            centralManager?.stopScan()
        }

        isRunning = false
        logger.info("Bluetooth LE scanning stopped")
    }

    /// Whether this manager is actively scanning.
    var isActive: Bool { isRunning }

    /// Whether Bluetooth hardware is present and authorized.
    var isAvailable: Bool {
        centralManager?.state == .poweredOn
    }

    // MARK: - CBCentralManagerDelegate

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        switch central.state {
        case .poweredOn:
            if isRunning {
                logger.info("Bluetooth powered on -- starting periodic scans")
                performScan()
                schedulePeriodic()
            }
        case .poweredOff:
            logger.warning("Bluetooth is disabled -- BT PoL parameter will not be met")
        case .unauthorized:
            logger.warning("Bluetooth permission not granted -- BT PoL parameter will not be met")
        case .unsupported:
            logger.warning("Bluetooth not available on this device")
        default:
            break
        }
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        // WHY: peripheral.identifier is a UUID assigned by iOS for this central.
        // It's stable per central-peripheral pair but different across devices,
        // providing a consistent identifier for hashing without exposing MACs.
        let hash = hashIdentifier(peripheral.identifier.uuidString)
        currentScanPeers.insert(hash)
    }

    // MARK: - Private

    private func schedulePeriodic() {
        scanTimer?.invalidate()
        scanTimer = Timer.scheduledTimer(withTimeInterval: Self.scanIntervalSeconds, repeats: true) { [weak self] _ in
            self?.performScan()
        }
    }

    private func performScan() {
        guard centralManager?.state == .poweredOn else { return }

        currentScanPeers.removeAll()

        // WHY: Passing nil for serviceUUIDs discovers all advertising peripherals.
        // For PoL we want the broadest possible snapshot of the BLE environment.
        centralManager?.scanForPeripherals(withServices: nil, options: [
            CBCentralManagerScanOptionAllowDuplicatesKey: false
        ])

        logger.debug("BLE scan started")

        // Stop after the scan window and report results.
        stopScanTimer?.invalidate()
        stopScanTimer = Timer.scheduledTimer(withTimeInterval: Self.scanDurationSeconds, repeats: false) { [weak self] _ in
            self?.finishScan()
        }
    }

    private func finishScan() {
        centralManager?.stopScan()

        let peerHashes = Array(currentScanPeers)
        if peerHashes.isEmpty {
            logger.debug("BLE scan completed -- no peers found")
            return
        }

        logger.debug("BLE scan completed -- \(peerHashes.count) peers discovered")
        onBluetoothScan?(peerHashes)
    }

    /// Hash a Bluetooth identifier to an opaque 8-byte UInt64.
    ///
    /// PRIVACY: Raw identifiers are never stored or sent across the FFI
    /// boundary. The hash is a one-way transformation that preserves the
    /// ability to detect "same device seen again" without revealing the
    /// actual identifier.
    private func hashIdentifier(_ identifier: String) -> UInt64 {
        let data = Data(identifier.utf8)
        let hash = SHA256.hash(data: data)

        // Take the first 8 bytes of the SHA-256 hash as a UInt64.
        var result: UInt64 = 0
        let bytes = Array(hash)
        for i in 0..<8 {
            result |= UInt64(bytes[i]) << (i * 8)
        }
        return result
    }
}
