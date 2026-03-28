import Foundation
import CoreMotion

// ============================================================================
// BarometerManager — CMAltimeter wrapper for pressure/altitude data
//
// Mirrors Android's BarometerManager.kt:
// - Relative altitude and atmospheric pressure data
// - Posts sensor data for presence score calculation
//
// WHY: Barometer data contributes +5 to the Composite Presence Score.
// It helps verify that the device is in a real physical environment
// (emulators cannot produce realistic barometric pressure readings).
//
// The barometer is an OPTIONAL sensor -- it is not required for the
// binary pass/fail threshold. Many budget phones lack a barometer.
// ============================================================================

final class BarometerManager: ObservableObject {

    private let logger = GratiaLogger(tag: "GratiaBarometer")

    private let altimeter = CMAltimeter()
    private var isRunning = false

    /// Most recent pressure reading in kPa.
    @Published private(set) var currentPressure: Double = 0

    /// Most recent relative altitude change in meters.
    @Published private(set) var relativeAltitude: Double = 0

    /// Whether the barometer is available on this device.
    var isAvailable: Bool {
        CMAltimeter.isRelativeAltitudeAvailable()
    }

    /// Start collecting barometric pressure data.
    func start() {
        guard !isRunning else { return }

        guard isAvailable else {
            logger.warning("Barometer not available on this device")
            return
        }

        altimeter.startRelativeAltitudeUpdates(to: .main) { [weak self] data, error in
            guard let self = self, let data = data else {
                if let error = error {
                    self?.logger.warning("Barometer error: \(error.localizedDescription)")
                }
                return
            }

            // WHY: CMAltitudeData provides relative altitude (meters) from the
            // start of updates and atmospheric pressure (kPa). Both are useful
            // for verifying real-world physical presence.
            self.currentPressure = data.pressure.doubleValue
            self.relativeAltitude = data.relativeAltitude.doubleValue
        }

        isRunning = true
        logger.info("Barometer tracking started")
    }

    /// Stop barometric data collection.
    func stop() {
        guard isRunning else { return }

        altimeter.stopRelativeAltitudeUpdates()
        isRunning = false
        logger.info("Barometer tracking stopped")
    }

    /// Whether this manager is actively collecting data.
    var isActive: Bool { isRunning }
}
