import Foundation
import CoreLocation

// ============================================================================
// LocationManager — Core Location wrapper for Proof of Life GPS attestation
//
// Mirrors Android's GpsManager.kt:
// - CLLocationManager for GPS fixes
// - Background location updates via significant location changes
// - Posts GpsUpdate sensor events to the bridge
//
// PRIVACY: Only lat/lon are captured and immediately forwarded as a
// SensorEvent. Raw location data is never persisted on disk.
// ============================================================================

final class LocationManager: NSObject, ObservableObject, CLLocationManagerDelegate {

    private let logger = GratiaLogger(tag: "GratiaLocationManager")

    private let locationManager = CLLocationManager()
    private var isRunning = false

    /// Callback for PoL sensor events.
    var onGpsUpdate: ((Float, Float) -> Void)?

    // WHY: 15-minute interval balances PoL requirement (at least one fix per day)
    // against battery consumption. Even one successful fix satisfies the GPS parameter.
    // On iOS, significant location change monitoring is more battery-efficient than
    // timed updates and fires on ~500m displacement.
    private static let desiredAccuracy = kCLLocationAccuracyHundredMeters

    // WHY: Coarse accuracy (city-block level) is sufficient for PoL geographic
    // plausibility checks. Fine location would be more power-hungry and privacy-invasive.
    private static let distanceFilter: CLLocationDistance = 100 // meters

    override init() {
        super.init()
        locationManager.delegate = self
        locationManager.desiredAccuracy = Self.desiredAccuracy
        locationManager.distanceFilter = Self.distanceFilter

        // WHY: Allow background location updates so PoL can get at least one
        // GPS fix even if the user hasn't opened the app today.
        locationManager.allowsBackgroundLocationUpdates = true

        // WHY: Pause updates automatically to save battery when the device
        // is stationary for an extended period. iOS will resume when movement
        // is detected.
        locationManager.pausesLocationUpdatesAutomatically = true
    }

    /// Start requesting location updates.
    ///
    /// Requests "when in use" authorization if not already granted.
    /// If permission is denied, logs a warning and returns — the PoL GPS
    /// parameter simply won't be satisfied.
    func start() {
        guard !isRunning else { return }

        let status = locationManager.authorizationStatus
        switch status {
        case .notDetermined:
            locationManager.requestWhenInUseAuthorization()
            // WHY: Don't start updates yet — wait for the delegate callback
            // in locationManagerDidChangeAuthorization.
            return
        case .denied, .restricted:
            logger.warning("Location permission denied -- GPS PoL parameter will not be met")
            return
        case .authorizedWhenInUse, .authorizedAlways:
            break
        @unknown default:
            break
        }

        startUpdates()
    }

    /// Stop location updates and release resources.
    func stop() {
        guard isRunning else { return }
        locationManager.stopUpdatingLocation()
        locationManager.stopMonitoringSignificantLocationChanges()
        isRunning = false
        logger.info("GPS tracking stopped")
    }

    /// Whether this manager is actively collecting location data.
    var isActive: Bool { isRunning }

    // MARK: - CLLocationManagerDelegate

    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        switch manager.authorizationStatus {
        case .authorizedWhenInUse, .authorizedAlways:
            if !isRunning {
                startUpdates()
            }
            // WHY: If we have "when in use" but want "always" for background PoL,
            // request the upgrade. The system will show a prompt the first time.
            if manager.authorizationStatus == .authorizedWhenInUse {
                locationManager.requestAlwaysAuthorization()
            }
        case .denied, .restricted:
            logger.warning("Location authorization denied")
            stop()
        default:
            break
        }
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let location = locations.last else { return }

        // WHY: Only use locations less than 30 minutes old. Stale locations
        // would not represent current geographic reality.
        let ageSeconds = -location.timestamp.timeIntervalSinceNow
        let maxAgeSeconds: TimeInterval = 30 * 60 // 30 minutes
        guard ageSeconds < maxAgeSeconds else {
            logger.debug("Ignoring stale location (age=\(Int(ageSeconds))s)")
            return
        }

        let lat = Float(location.coordinate.latitude)
        let lon = Float(location.coordinate.longitude)
        logger.debug("GPS fix obtained: lat=\(lat), lon=\(lon) (accuracy=\(location.horizontalAccuracy)m)")
        onGpsUpdate?(lat, lon)
    }

    func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        // WHY: Location failures are common and non-fatal (e.g., airplane mode,
        // underground). Log them but don't stop the manager — it will retry
        // automatically when conditions improve.
        logger.warning("Location update failed: \(error.localizedDescription)")
    }

    // MARK: - Private

    private func startUpdates() {
        // WHY: Start both standard updates (for foreground accuracy) and
        // significant location change monitoring (for background efficiency).
        // Significant changes survive app suspension and wake the app briefly.
        locationManager.startUpdatingLocation()
        locationManager.startMonitoringSignificantLocationChanges()
        isRunning = true
        logger.info("GPS tracking started")
    }
}
