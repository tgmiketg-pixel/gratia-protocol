import Foundation
import CoreMotion
import CoreLocation

// ============================================================================
// MagnetometerManager — Core Motion magnetometer + compass heading
//
// Mirrors Android's MagnetometerManager.kt:
// - CMMotionManager magnetometer data
// - CLLocationManager heading (compass) for presence score
//
// WHY: Magnetometer data contributes +4 to the Composite Presence Score.
// It provides additional environmental context that is difficult to
// emulate accurately (requires real magnetic field readings).
//
// The magnetometer is an OPTIONAL sensor -- not required for threshold.
// ============================================================================

final class MagnetometerManager: NSObject, ObservableObject, CLLocationManagerDelegate {

    private let logger = GratiaLogger(tag: "GratiaMagnetometer")

    private let motionManager = CMMotionManager()
    private let locationManager = CLLocationManager()
    private var isRunning = false

    /// Most recent magnetic field strength (microteslas).
    @Published private(set) var magneticFieldStrength: Double = 0

    /// Most recent compass heading (degrees from true north).
    @Published private(set) var heading: Double = 0

    /// Whether the magnetometer is available on this device.
    var isAvailable: Bool {
        motionManager.isMagnetometerAvailable
    }

    override init() {
        super.init()
        locationManager.delegate = self
    }

    /// Start collecting magnetometer data.
    func start() {
        guard !isRunning else { return }

        guard isAvailable else {
            logger.warning("Magnetometer not available on this device")
            return
        }

        // WHY: 1-second update interval is sufficient for presence score.
        // Magnetometer data doesn't change rapidly in normal use.
        motionManager.magnetometerUpdateInterval = 1.0

        motionManager.startMagnetometerUpdates(to: .main) { [weak self] data, error in
            guard let self = self, let data = data else { return }

            let field = data.magneticField
            self.magneticFieldStrength = sqrt(
                field.x * field.x + field.y * field.y + field.z * field.z
            )
        }

        // WHY: Compass heading provides a human-readable orientation metric
        // and verifies the device has real magnetic sensors.
        if CLLocationManager.headingAvailable() {
            locationManager.startUpdatingHeading()
        }

        isRunning = true
        logger.info("Magnetometer tracking started")
    }

    /// Stop magnetometer data collection.
    func stop() {
        guard isRunning else { return }

        motionManager.stopMagnetometerUpdates()
        locationManager.stopUpdatingHeading()
        isRunning = false
        logger.info("Magnetometer tracking stopped")
    }

    /// Whether this manager is actively collecting data.
    var isActive: Bool { isRunning }

    // MARK: - CLLocationManagerDelegate (heading)

    func locationManager(_ manager: CLLocationManager, didUpdateHeading newHeading: CLHeading) {
        // WHY: trueHeading is relative to true north (requires location fix).
        // magneticHeading works without GPS. We prefer true heading when available.
        if newHeading.trueHeading >= 0 {
            heading = newHeading.trueHeading
        } else {
            heading = newHeading.magneticHeading
        }
    }
}
