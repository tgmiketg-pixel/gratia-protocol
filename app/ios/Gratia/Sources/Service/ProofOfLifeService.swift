import Foundation
import BackgroundTasks
import UIKit

// ============================================================================
// ProofOfLifeService — Background task manager for passive PoL data collection
//
// Mirrors Android's ProofOfLifeService.kt:
// - Uses BGAppRefreshTask and BGProcessingTask for background execution
// - Collects sensor events periodically
// - Handles day rollover at midnight UTC
// - Submits sensor events to Rust core via bridge
//
// iOS Background Execution Strategy:
// Unlike Android which can run a persistent foreground service, iOS requires
// a combination of strategies for background sensor collection:
//
// 1. BGAppRefreshTask — runs ~every 15 minutes for brief sensor snapshots
// 2. BGProcessingTask — runs during overnight charging for day finalization
// 3. Significant location changes — wakes the app on ~500m displacement
// 4. Background push notifications — can trigger sensor collection
// 5. beginBackgroundTask — extends execution time when app enters background
//
// WHY this approach: iOS severely limits background execution to protect
// battery life. We use every available background execution mechanism to
// maximize PoL sensor collection coverage while respecting platform constraints.
// ============================================================================

final class ProofOfLifeService {

    // MARK: - Singleton

    static let shared = ProofOfLifeService()

    private let logger = GratiaLogger(tag: "GratiaPoLService")

    // MARK: - Background Task Identifiers

    // WHY: These must match the identifiers in Info.plist's
    // BGTaskSchedulerPermittedIdentifiers array.
    static let refreshTaskIdentifier = "io.gratia.app.pol-refresh"
    static let processingTaskIdentifier = "io.gratia.app.pol-day-finalization"

    // MARK: - Sensor Managers

    private var locationManager: LocationManager?
    private var motionManager: MotionManager?
    private var bluetoothManager: BluetoothManager?
    private var wifiManager: WifiManager?
    private var batteryManager: BatteryManager?
    private var barometerManager: BarometerManager?
    private var magnetometerManager: MagnetometerManager?
    private var lightSensorManager: LightSensorManager?
    private var nfcManager: NfcManager?

    private var isRunning = false
    private var midnightTimer: Timer?

    /// Background task ID for extended execution when app is backgrounded.
    private var backgroundTaskId: UIBackgroundTaskIdentifier = .invalid

    private init() {}

    // MARK: - Public API

    /// Register background tasks with the system.
    ///
    /// WHY: Must be called during application(_:didFinishLaunchingWithOptions:)
    /// or the SwiftUI app's init, BEFORE any background tasks are scheduled.
    /// The system requires registration before scheduling.
    func registerBackgroundTasks() {
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: Self.refreshTaskIdentifier,
            using: nil
        ) { [weak self] task in
            self?.handleRefreshTask(task as! BGAppRefreshTask)
        }

        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: Self.processingTaskIdentifier,
            using: nil
        ) { [weak self] task in
            self?.handleProcessingTask(task as! BGProcessingTask)
        }

        logger.info("Background tasks registered")
    }

    /// Initialize and start all sensor managers.
    ///
    /// Called when the app launches or returns to foreground.
    func startSensorCollection() {
        guard !isRunning else { return }
        guard GratiaCoreManager.shared.isInitialized else {
            logger.warning("GratiaCoreManager not initialized -- deferring sensor start")
            return
        }

        logger.info("Starting sensor collection")

        // Initialize sensor managers
        initializeSensors()

        // Schedule background tasks
        scheduleRefreshTask()
        scheduleProcessingTask()

        // Start midnight rollover timer
        startMidnightRolloverTimer()

        // WHY: Evaluate mining conditions immediately on start.
        // If the phone is already plugged in and charged above 80%, we need
        // to detect this and start mining right away.
        evaluateMiningConditions()

        isRunning = true
        logger.info("Sensor collection started")
    }

    /// Stop all sensor managers and cancel background tasks.
    func stopSensorCollection() {
        guard isRunning else { return }

        locationManager?.stop()
        motionManager?.stop()
        bluetoothManager?.stop()
        wifiManager?.stop()
        batteryManager?.stop()
        barometerManager?.stop()
        magnetometerManager?.stop()
        lightSensorManager?.stop()

        midnightTimer?.invalidate()
        midnightTimer = nil

        isRunning = false
        logger.info("Sensor collection stopped")
    }

    /// Called when the app enters the background.
    ///
    /// WHY: Request extended background execution time so sensors can
    /// complete their current collection cycle before iOS suspends the app.
    func handleAppDidEnterBackground() {
        backgroundTaskId = UIApplication.shared.beginBackgroundTask { [weak self] in
            // WHY: Expiration handler — called when iOS is about to suspend.
            // We must end the task cleanly to avoid being penalized by the system.
            self?.endBackgroundTask()
        }
        logger.info("Background task started (id=\(backgroundTaskId.rawValue))")
    }

    /// Called when the app returns to foreground.
    func handleAppWillEnterForeground() {
        endBackgroundTask()

        // Restart any stopped managers
        if !isRunning {
            startSensorCollection()
        }
    }

    // MARK: - Sensor Initialization

    private func initializeSensors() {
        // GPS / Location
        locationManager = LocationManager()
        locationManager?.onGpsUpdate = { [weak self] lat, lon in
            self?.submitEvent(.gpsUpdate(lat: lat, lon: lon))
        }
        locationManager?.start()

        // Accelerometer / Motion
        motionManager = MotionManager()
        motionManager?.onMotion = { [weak self] in
            self?.submitEvent(.motion)
        }
        motionManager?.onOrientationChange = { [weak self] in
            self?.submitEvent(.orientationChange)
        }
        motionManager?.start()

        // Bluetooth
        bluetoothManager = BluetoothManager()
        bluetoothManager?.onBluetoothScan = { [weak self] peerHashes in
            self?.submitEvent(.bluetoothScan(peerHashes: peerHashes))
        }
        bluetoothManager?.start()

        // Wi-Fi
        wifiManager = WifiManager()
        wifiManager?.onWifiScan = { [weak self] bssidHashes in
            self?.submitEvent(.wifiScan(bssidHashes: bssidHashes))
        }
        wifiManager?.start()

        // Battery
        batteryManager = BatteryManager()
        batteryManager?.onChargeEvent = { [weak self] isCharging in
            self?.submitEvent(.chargeEvent(isCharging: isCharging))
        }
        batteryManager?.onPowerStateChanged = { [weak self] isPluggedIn, batteryPercent in
            self?.handlePowerStateChange(isPluggedIn: isPluggedIn, batteryPercent: batteryPercent)
        }
        batteryManager?.start()

        // Optional sensors (boost Presence Score, not required)
        barometerManager = BarometerManager()
        barometerManager?.start()

        magnetometerManager = MagnetometerManager()
        magnetometerManager?.start()

        lightSensorManager = LightSensorManager()
        lightSensorManager?.start()

        nfcManager = NfcManager()

        // WHY: Register for screen unlock notifications.
        // iOS does not have a direct "user unlocked" notification like Android's
        // ACTION_USER_PRESENT. We approximate using UIApplication lifecycle:
        // - didBecomeActive fires when the app becomes active (including after unlock)
        // - protectedDataDidBecomeAvailable fires when the device is unlocked
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleDeviceUnlocked),
            name: UIApplication.protectedDataDidBecomeAvailableNotification,
            object: nil
        )

        logger.info("All sensor managers initialized")
    }

    // MARK: - Event Submission

    /// Forward a sensor event to the Rust core via GratiaCoreManager.
    private func submitEvent(_ event: SensorEvent) {
        guard GratiaCoreManager.shared.isInitialized else {
            logger.warning("GratiaCoreManager not initialized -- dropping sensor event: \(event.type)")
            return
        }

        logger.info("PoL sensor event: \(event.type)")

        Task { @MainActor in
            do {
                try GratiaCoreManager.shared.submitSensorEvent(event)
            } catch {
                logger.error("Failed to submit sensor event [\(event.type)]: \(error.localizedDescription)")
            }
        }
    }

    @objc private func handleDeviceUnlocked() {
        submitEvent(.unlock)
    }

    // MARK: - Mining Condition Evaluation

    private func handlePowerStateChange(isPluggedIn: Bool, batteryPercent: Int) {
        evaluateMiningConditions()
    }

    /// Check whether mining conditions are met and start/stop MiningService.
    private func evaluateMiningConditions() {
        guard GratiaCoreManager.shared.isInitialized else { return }

        let bm = batteryManager
        let isPluggedIn = bm?.isPluggedIn ?? false
        let batteryPercent = bm?.batteryPercent ?? 0

        Task { @MainActor in
            do {
                let status = try GratiaCoreManager.shared.updatePowerState(
                    isPluggedIn: isPluggedIn,
                    batteryPercent: batteryPercent
                )

                switch status.state {
                case "mining":
                    MiningService.shared.startMining()
                case "battery_low", "proof_of_life":
                    MiningService.shared.stopMining()
                case "throttled":
                    logger.debug("Mining throttled due to thermal conditions")
                case "pending_activation":
                    logger.debug("Mining pending -- waiting for PoL or stake")
                default:
                    break
                }
            } catch {
                logger.error("Error evaluating mining conditions: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - Background Tasks

    /// Handle the periodic refresh task (~every 15 minutes).
    private func handleRefreshTask(_ task: BGAppRefreshTask) {
        logger.info("Background refresh task executing")

        // Schedule the next refresh
        scheduleRefreshTask()

        task.expirationHandler = {
            // WHY: The system is about to kill this task. Clean up gracefully.
            task.setTaskCompleted(success: false)
        }

        // Perform a quick sensor snapshot
        Task { @MainActor in
            do {
                // Log current PoL status
                let status = try GratiaCoreManager.shared.getProofOfLifeStatus()
                logger.info("Background PoL status: \(status.parametersMet.count)/8 parameters met")
                task.setTaskCompleted(success: true)
            } catch {
                logger.error("Background refresh failed: \(error.localizedDescription)")
                task.setTaskCompleted(success: false)
            }
        }
    }

    /// Handle the processing task (for day finalization during charging).
    private func handleProcessingTask(_ task: BGProcessingTask) {
        logger.info("Background processing task executing (day finalization)")

        task.expirationHandler = {
            task.setTaskCompleted(success: false)
        }

        performDayFinalization()
        task.setTaskCompleted(success: true)

        // Schedule the next processing task
        scheduleProcessingTask()
    }

    private func scheduleRefreshTask() {
        let request = BGAppRefreshTaskRequest(identifier: Self.refreshTaskIdentifier)
        // WHY: 15-minute earliest begin date. iOS may delay this further based
        // on device usage patterns and battery state.
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)

        do {
            try BGTaskScheduler.shared.submit(request)
            logger.debug("Refresh task scheduled")
        } catch {
            logger.error("Failed to schedule refresh task: \(error.localizedDescription)")
        }
    }

    private func scheduleProcessingTask() {
        let request = BGProcessingTaskRequest(identifier: Self.processingTaskIdentifier)
        // WHY: requiresExternalPower = true means this task only runs when charging.
        // Day finalization is not time-critical and can wait until the phone is
        // plugged in, matching our "battery health sacred" principle.
        request.requiresExternalPower = true
        request.requiresNetworkConnectivity = false

        do {
            try BGTaskScheduler.shared.submit(request)
            logger.debug("Processing task scheduled")
        } catch {
            logger.error("Failed to schedule processing task: \(error.localizedDescription)")
        }
    }

    // MARK: - Midnight Rollover

    /// Start a timer that fires at midnight UTC for day finalization.
    ///
    /// WHY: Precise midnight execution ensures consistent day boundaries
    /// across all nodes in the network. The timer is recalculated each day.
    private func startMidnightRolloverTimer() {
        scheduleMidnightTimer()
    }

    private func scheduleMidnightTimer() {
        midnightTimer?.invalidate()

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!

        guard let nextMidnight = calendar.nextDate(
            after: Date(),
            matching: DateComponents(hour: 0, minute: 0, second: 0),
            matchingPolicy: .nextTime
        ) else {
            logger.error("Could not calculate next midnight UTC")
            return
        }

        let interval = nextMidnight.timeIntervalSinceNow
        logger.debug("Next day finalization in \(Int(interval))s")

        midnightTimer = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in
            self?.performDayFinalization()
            self?.scheduleMidnightTimer() // Schedule next midnight
        }
    }

    /// Finalize the current day's Proof of Life via the Rust core.
    private func performDayFinalization() {
        guard GratiaCoreManager.shared.isInitialized else {
            logger.error("Cannot finalize day -- GratiaCoreManager not initialized")
            return
        }

        Task { @MainActor in
            do {
                let isValid = try GratiaCoreManager.shared.finalizeDay()
                if isValid {
                    logger.info("Day finalized: VALID")
                } else {
                    logger.warning("Day finalized: INVALID -- some PoL parameters were not met")
                }
            } catch {
                logger.error("Error finalizing day: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - Private

    private func endBackgroundTask() {
        guard backgroundTaskId != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTaskId)
        backgroundTaskId = .invalid
        logger.debug("Background task ended")
    }
}
