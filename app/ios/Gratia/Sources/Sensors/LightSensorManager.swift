import Foundation
import UIKit

// ============================================================================
// LightSensorManager — Ambient light level approximation
//
// WHY iOS doesn't expose ambient light directly:
// Unlike Android, iOS does not provide public API access to the ambient light
// sensor (ALS). The ALS is used internally by iOS for auto-brightness but
// is not exposed through any public framework (Core Motion, UIKit, etc.).
//
// Workaround: We use UIScreen.main.brightness as a proxy. When auto-brightness
// is enabled, the screen brightness correlates with ambient light levels.
// This is an imperfect proxy but sufficient for Presence Score contribution.
//
// The ambient light sensor contributes +3 to the Composite Presence Score.
// It is an OPTIONAL sensor -- not required for the binary pass/fail threshold.
//
// This limitation is documented in the CLAUDE.md project structure.
// ============================================================================

final class LightSensorManager: ObservableObject {

    private let logger = GratiaLogger(tag: "GratiaLightSensor")

    private var isRunning = false
    private var pollingTimer: Timer?

    /// Most recent screen brightness level (0.0 to 1.0).
    /// When auto-brightness is enabled, this approximates ambient light.
    @Published private(set) var brightnessLevel: Float = 0

    /// Whether this sensor can provide useful data.
    ///
    /// WHY: Always returns true on iOS because UIScreen.brightness is always
    /// available. However, the data is only a useful ambient light proxy when
    /// auto-brightness is enabled (which we cannot detect programmatically).
    var isAvailable: Bool { true }

    /// Start polling screen brightness at regular intervals.
    ///
    /// WHY: UIScreen.brightness does not have a change notification. We poll
    /// every 60 seconds, which is frequent enough for Presence Score calculation
    /// while consuming negligible resources.
    func start() {
        guard !isRunning else { return }

        // Read initial value
        brightnessLevel = Float(UIScreen.main.brightness)

        // WHY: 60-second polling interval. Ambient light levels change slowly
        // in most environments. Faster polling would not meaningfully improve
        // the Presence Score calculation.
        pollingTimer = Timer.scheduledTimer(withTimeInterval: 60.0, repeats: true) { [weak self] _ in
            guard let self = self else { return }
            self.brightnessLevel = Float(UIScreen.main.brightness)
        }

        // WHY: Also observe the brightness change notification for immediate updates.
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(brightnessDidChange),
            name: UIScreen.brightnessDidChangeNotification,
            object: nil
        )

        isRunning = true
        logger.info("Light sensor proxy started (using screen brightness)")
    }

    /// Stop polling.
    func stop() {
        guard isRunning else { return }

        pollingTimer?.invalidate()
        pollingTimer = nil
        NotificationCenter.default.removeObserver(self, name: UIScreen.brightnessDidChangeNotification, object: nil)
        isRunning = false
        logger.info("Light sensor proxy stopped")
    }

    /// Whether this manager is actively collecting data.
    var isActive: Bool { isRunning }

    // MARK: - Private

    @objc private func brightnessDidChange(_ notification: Notification) {
        brightnessLevel = Float(UIScreen.main.brightness)
    }
}
