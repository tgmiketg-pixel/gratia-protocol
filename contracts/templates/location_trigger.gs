// GratiaScript — Location Trigger Contract
//
// Activates when a user enters a geographic zone. Use cases:
// - Check-in rewards at events or businesses
// - Geofenced content unlocking
// - Location-based token airdrops
//
// Mobile-native opcodes used: @location()

contract LocationTrigger {
    let triggerLat: f32 = 0.0;
    let triggerLon: f32 = 0.0;
    let radius: f32 = 0.001;  // ~100 meters in degrees

    function configure(lat: f32, lon: f32, r: f32): void {
        triggerLat = lat;
        triggerLon = lon;
        radius = r;
    }

    function check(): bool {
        let loc = @location();
        let dlat = loc.lat - triggerLat;
        let dlon = loc.lon - triggerLon;
        let dist = dlat * dlat + dlon * dlon;
        if (dist < radius * radius) {
            emit("triggered", "User entered zone");
            return true;
        }
        return false;
    }

    function getRadius(): f32 {
        return radius;
    }
}
