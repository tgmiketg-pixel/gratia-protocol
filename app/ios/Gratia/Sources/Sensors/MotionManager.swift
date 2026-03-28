import Foundation
import CoreMotion

// ============================================================================
// MotionManager — Core Motion wrapper for accelerometer, gyroscope, activity
//
// Mirrors Android's AccelerometerManager.kt:
// - CMMotionManager for accelerometer + gyroscope
// - CMMotionActivityManager for activity detection
// - Orientation change detection via device motion
// - Posts Motion and OrientationChange sensor events
//
// PRIVACY: Raw accelerometer readings are never stored or transmitted.
// Only the boolean "human motion detected" / "orientation changed" flags
// cross the FFI boundary.
// ============================================================================

final class MotionManager: ObservableObject {

    private let logger = GratiaLogger(tag: "GratiaMotionManager")

    private let motionManager = CMMotionManager()
    private let activityManager = CMMotionActivityManager()
    private var isRunning = false

    /// Callbacks for PoL sensor events.
    var onMotion: (() -> Void)?
    var onOrientationChange: (() -> Void)?

    // Gravity baseline for orientation change detection.
    private var prevGravityX: Double = 0
    private var prevGravityY: Double = 0
    private var prevGravityZ: Double = 0
    private var baselineEstablished = false
    private var baselineSamplesCollected = 0

    // Debounce timestamps
    private var lastMotionEventTime: Date = .distantPast
    private var lastOrientationEventTime: Date = .distantPast

    // WHY: A magnitude above this threshold indicates the phone is being carried
    // or handled by a human rather than sitting on a desk.
    private static let motionThreshold: Double = 1.5 // m/s^2 above gravity

    // WHY: Orientation is considered changed when the dominant gravity axis
    // shifts by more than 3 m/s^2. This detects picking up the phone,
    // rotating it, or setting it down.
    private static let orientationThreshold: Double = 3.0

    // WHY: Debounce prevents flooding the PoL engine with redundant events.
    // One motion detection per 5 minutes is sufficient for the daily PoL flag.
    private static let motionDebounceInterval: TimeInterval = 5 * 60 // 5 minutes

    // WHY: Orientation events are rarer and more meaningful -- 2-minute debounce.
    private static let orientationDebounceInterval: TimeInterval = 2 * 60 // 2 minutes

    // WHY: We need a few samples to compute a stable baseline gravity vector.
    // 10 samples at ~50ms each takes ~0.5 seconds.
    private static let baselineSampleCount = 10

    /// Start collecting accelerometer and device motion data.
    ///
    /// Uses a low update interval (~5 Hz) to minimize battery impact while
    /// still detecting motion patterns throughout the day.
    func start() {
        guard !isRunning else { return }

        guard motionManager.isAccelerometerAvailable else {
            logger.warning("Accelerometer not available on this device")
            return
        }

        guard motionManager.isDeviceMotionAvailable else {
            logger.warning("Device motion not available -- falling back to raw accelerometer")
            startRawAccelerometer()
            return
        }

        // WHY: 0.2-second interval (~5 Hz) is the lowest practical sampling rate.
        // We don't need high-frequency data -- just enough to detect motion
        // patterns over the course of the day.
        motionManager.deviceMotionUpdateInterval = 0.2

        motionManager.startDeviceMotionUpdates(to: .main) { [weak self] motion, error in
            guard let self = self, let motion = motion else {
                if let error = error {
                    self?.logger.warning("Device motion error: \(error.localizedDescription)")
                }
                return
            }
            self.processDeviceMotion(motion)
        }

        // Start activity recognition for additional context
        if CMMotionActivityManager.isActivityAvailable() {
            activityManager.startActivityUpdates(to: .main) { [weak self] activity in
                guard let activity = activity else { return }
                // WHY: Walking, running, or cycling all count as human-consistent motion.
                // Stationary + automotive does not (could be a phone farm in a car).
                if activity.walking || activity.running || activity.cycling {
                    self?.emitMotionIfDebounced()
                }
            }
        }

        isRunning = true
        logger.info("Motion tracking started (device motion + activity)")
    }

    /// Stop motion tracking and release resources.
    func stop() {
        guard isRunning else { return }

        motionManager.stopDeviceMotionUpdates()
        motionManager.stopAccelerometerUpdates()
        activityManager.stopActivityUpdates()

        isRunning = false
        baselineEstablished = false
        baselineSamplesCollected = 0
        logger.info("Motion tracking stopped")
    }

    /// Whether this manager is actively collecting data.
    var isActive: Bool { isRunning }

    /// Whether the accelerometer sensor is present on this device.
    var isAvailable: Bool { motionManager.isAccelerometerAvailable }

    // MARK: - Private

    /// Process a device motion update — extracts linear acceleration and gravity.
    ///
    /// WHY: CMDeviceMotion separates gravity from user acceleration automatically
    /// (unlike raw accelerometer data), making our classification simpler and
    /// more accurate than the Android version which needs a manual low-pass filter.
    private func processDeviceMotion(_ motion: CMDeviceMotion) {
        let gravity = motion.gravity
        let userAccel = motion.userAcceleration

        // Establish gravity baseline
        if !baselineEstablished {
            baselineSamplesCollected += 1
            if baselineSamplesCollected >= Self.baselineSampleCount {
                baselineEstablished = true
                prevGravityX = gravity.x
                prevGravityY = gravity.y
                prevGravityZ = gravity.z
                logger.debug("Gravity baseline established: (\(gravity.x), \(gravity.y), \(gravity.z))")
            }
            return
        }

        // Linear acceleration magnitude (already separated from gravity by CoreMotion)
        let linearMagnitude = sqrt(
            userAccel.x * userAccel.x +
            userAccel.y * userAccel.y +
            userAccel.z * userAccel.z
        )

        // Motion detection
        if linearMagnitude > Self.motionThreshold {
            emitMotionIfDebounced()
        }

        // Orientation change detection
        let gravityShiftX = gravity.x - prevGravityX
        let gravityShiftY = gravity.y - prevGravityY
        let gravityShiftZ = gravity.z - prevGravityZ
        let gravityShiftMag = sqrt(
            gravityShiftX * gravityShiftX +
            gravityShiftY * gravityShiftY +
            gravityShiftZ * gravityShiftZ
        )

        if gravityShiftMag > Self.orientationThreshold {
            let now = Date()
            if now.timeIntervalSince(lastOrientationEventTime) > Self.orientationDebounceInterval {
                lastOrientationEventTime = now
                prevGravityX = gravity.x
                prevGravityY = gravity.y
                prevGravityZ = gravity.z
                logger.debug("Orientation change detected (shift=\(gravityShiftMag))")
                onOrientationChange?()
            }
        }
    }

    /// Fallback: raw accelerometer when device motion is unavailable.
    private func startRawAccelerometer() {
        motionManager.accelerometerUpdateInterval = 0.2
        motionManager.startAccelerometerUpdates(to: .main) { [weak self] data, error in
            guard let self = self, let data = data else { return }
            // WHY: Without device motion, we need to manually separate gravity.
            // This is a simplified version — less accurate than CMDeviceMotion
            // but sufficient for PoL motion detection.
            let mag = sqrt(data.acceleration.x * data.acceleration.x +
                          data.acceleration.y * data.acceleration.y +
                          data.acceleration.z * data.acceleration.z)
            // Gravity is ~1.0 in CMAcceleration (measured in g, not m/s^2)
            let linearMag = abs(mag - 1.0)
            if linearMag > 0.15 { // ~1.5 m/s^2 equivalent in g
                self.emitMotionIfDebounced()
            }
        }
        isRunning = true
        logger.info("Raw accelerometer tracking started (fallback)")
    }

    private func emitMotionIfDebounced() {
        let now = Date()
        guard now.timeIntervalSince(lastMotionEventTime) > Self.motionDebounceInterval else { return }
        lastMotionEventTime = now
        logger.debug("Human motion detected")
        onMotion?()
    }
}
