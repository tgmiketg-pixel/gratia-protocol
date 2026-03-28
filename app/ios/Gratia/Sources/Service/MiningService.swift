import Foundation
import UIKit

// ============================================================================
// MiningService — Mining background service
//
// Mirrors Android's MiningService.kt:
// - Monitors battery state
// - Starts/stops mining based on charging + 80% battery
// - Uses UIApplication background task for extended execution
// - Thermal monitoring via ProcessInfo.thermalState
//
// iOS Mining Execution Strategy:
// Unlike Android where a foreground service can run indefinitely, iOS limits
// background execution. Mining on iOS works as follows:
//
// 1. Foreground: Mining runs continuously while the app is open
// 2. Background: Uses beginBackgroundTask for ~30 seconds of grace period
// 3. Extended background: Relies on the Rust core to handle mining ticks
//    via the ProofOfLifeService's background tasks
//
// WHY: The flat-rate reward (1 GRAT/minute) means missed background minutes
// are lost earnings, not security issues. The protocol is designed so that
// mining is most efficient when the user has the app open while charging,
// which aligns with iOS's foreground-first execution model.
// ============================================================================

final class MiningService: ObservableObject {

    // MARK: - Singleton

    static let shared = MiningService()

    private let logger = GratiaLogger(tag: "GratiaMiningService")

    /// Whether mining is currently active.
    @Published private(set) var isMining = false

    private var miningTimer: Timer?
    private var thermalObserver: NSObjectProtocol?
    private var backgroundTaskId: UIBackgroundTaskIdentifier = .invalid

    // WHY: 60-second tick interval matches the protocol's flat reward rate
    // of 1 GRAT per minute of active mining.
    private static let miningTickIntervalSeconds: TimeInterval = 60

    private init() {
        // WHY: Monitor thermal state changes to throttle mining when the
        // device gets too hot. This is mandatory per the protocol spec:
        // "Thermal management throttles workload if CPU gets too hot."
        thermalObserver = NotificationCenter.default.addObserver(
            forName: ProcessInfo.thermalStateDidChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.handleThermalStateChange()
        }
    }

    deinit {
        if let observer = thermalObserver {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    // MARK: - Public API

    /// Start mining. Called when all conditions are met:
    /// plugged in, battery >= 80%, valid PoL, minimum stake.
    func startMining() {
        guard !isMining else { return }

        // WHY: Check thermal state before starting. If the device is already
        // seriously overheated, don't start mining — it would make things worse.
        let thermalState = ProcessInfo.processInfo.thermalState
        if thermalState == .critical {
            logger.warning("Cannot start mining -- device is critically hot")
            return
        }

        isMining = true

        // WHY: Timer fires every 60 seconds to credit the mining reward.
        // Each tick calls GratiaCoreManager.tickMiningReward() which credits
        // 1 GRAT (1,000,000 Lux) to the wallet.
        miningTimer = Timer.scheduledTimer(withTimeInterval: Self.miningTickIntervalSeconds, repeats: true) { [weak self] _ in
            self?.performMiningTick()
        }

        // WHY: Request background execution time so mining continues briefly
        // if the user switches apps while plugged in.
        beginBackgroundMining()

        // Post notification for UI updates
        NotificationCenter.default.post(name: .miningDidStart, object: nil)

        logger.info("Mining started")
    }

    /// Stop mining. Called when:
    /// - User manually stops mining
    /// - Phone is unplugged
    /// - Battery drops below 80%
    /// - PoL becomes invalid
    func stopMining() {
        guard isMining else { return }

        isMining = false
        miningTimer?.invalidate()
        miningTimer = nil
        endBackgroundMining()

        // Post notification for UI updates
        NotificationCenter.default.post(name: .miningDidStop, object: nil)

        logger.info("Mining stopped")
    }

    // MARK: - Mining Tick

    /// Credit the wallet with one minute of mining rewards.
    private func performMiningTick() {
        guard isMining else { return }

        // WHY: Check thermal state on each tick. If the device has overheated
        // since mining started, pause the workload.
        let thermalState = ProcessInfo.processInfo.thermalState
        if thermalState == .critical || thermalState == .serious {
            logger.warning("Thermal throttle: \(thermalState.rawValue) -- pausing mining tick")
            return
        }

        Task { @MainActor in
            do {
                let newBalance = try GratiaCoreManager.shared.tickMiningReward()
                logger.debug("Mining tick: new balance = \(newBalance) Lux")
            } catch {
                logger.error("Mining tick failed: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - Thermal Management

    /// Handle thermal state changes from the system.
    ///
    /// WHY: ProcessInfo.thermalState has four levels:
    /// - .nominal: Normal operation
    /// - .fair: Slightly elevated, mining continues normally
    /// - .serious: Hot — reduce workload (skip some ticks)
    /// - .critical: Emergency — stop mining immediately
    ///
    /// Battery health is sacred: we never degrade the user's device.
    private func handleThermalStateChange() {
        let state = ProcessInfo.processInfo.thermalState

        switch state {
        case .nominal, .fair:
            logger.debug("Thermal state: \(state.rawValue) -- mining continues normally")
        case .serious:
            logger.warning("Thermal state: serious -- mining workload reduced")
            // WHY: We don't stop mining on .serious, just reduce the workload.
            // The tick handler already checks thermal state and skips ticks.
        case .critical:
            logger.warning("Thermal state: CRITICAL -- stopping mining")
            if isMining {
                stopMining()
            }
        @unknown default:
            break
        }
    }

    // MARK: - Background Execution

    /// Request extended background execution time for mining.
    ///
    /// WHY: iOS gives ~30 seconds of background execution when the app
    /// enters the background. This is enough for one mining tick (which
    /// takes milliseconds) but not for continuous mining.
    private func beginBackgroundMining() {
        guard backgroundTaskId == .invalid else { return }

        backgroundTaskId = UIApplication.shared.beginBackgroundTask(withName: "GratiaMining") { [weak self] in
            // Expiration handler — clean up and end the task
            self?.logger.info("Background mining time expired")
            self?.endBackgroundMining()
        }
    }

    private func endBackgroundMining() {
        guard backgroundTaskId != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTaskId)
        backgroundTaskId = .invalid
    }
}

// MARK: - Notification Names

extension Notification.Name {
    static let miningDidStart = Notification.Name("io.gratia.miningDidStart")
    static let miningDidStop = Notification.Name("io.gratia.miningDidStop")
}
