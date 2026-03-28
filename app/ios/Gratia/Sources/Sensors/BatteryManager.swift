import Foundation
import UIKit

// ============================================================================
// BatteryManager — UIDevice battery monitoring for PoL and mining mode
//
// Mirrors Android's BatteryManager.kt:
// - Battery level and charging state monitoring
// - Triggers mining mode check (plugged in + 80%)
// - Posts ChargeEvent sensor events
//
// No special permissions required -- battery state is available to all apps.
// ============================================================================

final class BatteryManager: ObservableObject {

    private let logger = GratiaLogger(tag: "GratiaBattery")

    /// Current battery percentage (0-100). Updated on every change notification.
    @Published private(set) var batteryPercent: Int = 0

    /// Whether the phone is currently connected to a power source.
    @Published private(set) var isPluggedIn: Bool = false

    private var isRunning = false

    /// Callback for PoL charge cycle events.
    var onChargeEvent: ((Bool) -> Void)?

    /// Callback for power state changes (used by MiningService).
    var onPowerStateChanged: ((Bool, Int) -> Void)?

    /// Start monitoring battery and charging state.
    func start() {
        guard !isRunning else { return }

        // WHY: Enable battery monitoring on UIDevice. This must be set to true
        // before reading batteryLevel or batteryState. It's disabled by default
        // to save power on devices that don't need it.
        UIDevice.current.isBatteryMonitoringEnabled = true

        // Read initial state
        updateBatteryState()

        // WHY: NotificationCenter observers for battery level and state changes.
        // These fire on every percentage change (level) and plug/unplug (state).
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(batteryLevelDidChange),
            name: UIDevice.batteryLevelDidChangeNotification,
            object: nil
        )

        NotificationCenter.default.addObserver(
            self,
            selector: #selector(batteryStateDidChange),
            name: UIDevice.batteryStateDidChangeNotification,
            object: nil
        )

        isRunning = true
        logger.info("Battery monitoring started (level=\(batteryPercent)%, plugged=\(isPluggedIn))")
    }

    /// Stop monitoring and remove observers.
    func stop() {
        guard isRunning else { return }

        NotificationCenter.default.removeObserver(self, name: UIDevice.batteryLevelDidChangeNotification, object: nil)
        NotificationCenter.default.removeObserver(self, name: UIDevice.batteryStateDidChangeNotification, object: nil)

        UIDevice.current.isBatteryMonitoringEnabled = false
        isRunning = false
        logger.info("Battery monitoring stopped")
    }

    /// Whether this manager is actively monitoring.
    var isActive: Bool { isRunning }

    /// Check whether mining conditions are met right now.
    /// Mining requires: plugged in AND battery >= 80%.
    var isMiningEligible: Bool { isPluggedIn && batteryPercent >= 80 }

    // MARK: - Notification Handlers

    @objc private func batteryLevelDidChange(_ notification: Notification) {
        updateBatteryState()
    }

    @objc private func batteryStateDidChange(_ notification: Notification) {
        let wasPluggedIn = isPluggedIn
        updateBatteryState()

        // WHY: Detect plug/unplug transitions and emit PoL charge cycle events.
        if isPluggedIn != wasPluggedIn {
            logger.debug("Charging state changed: plugged=\(isPluggedIn)")
            onChargeEvent?(isPluggedIn)
        }
    }

    // MARK: - Private

    private func updateBatteryState() {
        let device = UIDevice.current

        // WHY: UIDevice.batteryLevel returns -1.0 if monitoring is disabled
        // or the value is unknown. We clamp to 0-100.
        let level = device.batteryLevel
        batteryPercent = level >= 0 ? Int(level * 100) : 0

        // WHY: .charging and .full both mean power is connected.
        // .unplugged means no power. .unknown means monitoring is off.
        let state = device.batteryState
        isPluggedIn = (state == .charging || state == .full)

        onPowerStateChanged?(isPluggedIn, batteryPercent)
    }
}
