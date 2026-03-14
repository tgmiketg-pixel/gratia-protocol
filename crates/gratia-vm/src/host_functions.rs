//! Mobile-native host functions (opcodes) for GratiaVM.
//!
//! These functions are exposed to smart contracts running inside the WASM
//! sandbox. They provide access to Gratia-specific data that is unique to
//! a mobile-native blockchain: GPS location, Bluetooth/Wi-Fi proximity,
//! Proof of Life presence scores, and physical sensor readings.
//!
//! All sensor data returned by host functions is **pre-processed and coarsened
//! on-device** before being made available to contracts. Contracts never receive
//! raw sensor data — only aggregated, privacy-preserving summaries.

use serde::{Deserialize, Serialize};

use gratia_core::types::{Address, GeoLocation, Lux};

// ============================================================================
// Sensor Types for Host Functions
// ============================================================================

/// Types of sensor data available to smart contracts via @sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SensorType {
    /// Barometric pressure (hPa).
    Barometer,
    /// Ambient light level (lux, the photometric unit — not GRAT Lux).
    AmbientLight,
    /// Magnetometer heading (degrees, 0-360).
    Magnetometer,
    /// Accelerometer magnitude (m/s^2, scalar — not raw xyz).
    Accelerometer,
    /// Gyroscope rotation rate (rad/s, scalar magnitude).
    Gyroscope,
}

/// A sensor reading returned by the @sensor host function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReading {
    /// Which sensor produced this reading.
    pub sensor_type: SensorType,
    /// The scalar value of the reading.
    /// Units depend on sensor_type (see SensorType docs).
    pub value: f64,
    /// Unix timestamp (seconds) when this reading was captured.
    pub timestamp_secs: u64,
    /// Whether the reading is considered fresh (taken within the last 60 seconds).
    pub is_fresh: bool,
}

// ============================================================================
// Host Environment
// ============================================================================

/// The execution context provided to contracts via host functions.
///
/// This struct is populated by the node before contract execution begins.
/// It contains all the data that host functions can return to the contract.
/// None of this data is raw — it has been coarsened and privacy-filtered
/// on-device before reaching this point.
#[derive(Debug, Clone)]
pub struct HostEnvironment {
    // -- Blockchain context --

    /// Current block height.
    pub block_height: u64,
    /// Current block timestamp (Unix seconds).
    pub block_timestamp: u64,
    /// Address of the account that initiated this contract call.
    pub caller_address: Address,
    /// Balance of the caller (in Lux).
    pub caller_balance: Lux,

    // -- Mobile-native context (@location) --

    /// Coarse GPS location of the executing node.
    /// None if the node has not provided location or has privacy restrictions.
    pub location: Option<GeoLocation>,

    // -- Mobile-native context (@proximity) --

    /// Number of nearby Bluetooth/Wi-Fi peers detected by the executing node.
    /// WHY: Contracts get a count, not identities, to preserve peer privacy.
    pub nearby_peer_count: u32,

    // -- Mobile-native context (@presence) --

    /// The executing node's Composite Presence Score (40-100).
    /// WHY: This is already public (used for VRF weighting), so exposing it
    /// to contracts does not leak additional information.
    pub presence_score: u8,

    // -- Mobile-native context (@sensor) --

    /// Latest sensor readings available on this device.
    /// The set of available sensors varies by device — contracts must
    /// handle missing sensors gracefully.
    pub sensor_readings: Vec<SensorReading>,

    // -- Contract storage (key-value) --

    /// Contract-local storage, keyed by 32-byte slot.
    /// In a real implementation, this would be backed by RocksDB via the
    /// state layer. Here we define the interface.
    storage: std::collections::HashMap<[u8; 32], Vec<u8>>,

    // -- Event log --

    /// Events emitted during execution. Collected here and included in
    /// the transaction receipt.
    pub events: Vec<ContractEvent>,
}

/// An event emitted by a contract during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractEvent {
    /// The contract that emitted this event.
    pub contract_address: Address,
    /// Event topic (for filtering/indexing).
    pub topic: String,
    /// Serialized event data.
    pub data: Vec<u8>,
}

impl HostEnvironment {
    /// Create a new HostEnvironment with the given blockchain context.
    pub fn new(
        block_height: u64,
        block_timestamp: u64,
        caller_address: Address,
        caller_balance: Lux,
    ) -> Self {
        HostEnvironment {
            block_height,
            block_timestamp,
            caller_address,
            caller_balance,
            location: None,
            nearby_peer_count: 0,
            presence_score: 40, // Minimum score (core sensors only)
            sensor_readings: Vec::new(),
            storage: std::collections::HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Set the node's coarse location.
    pub fn with_location(mut self, location: GeoLocation) -> Self {
        self.location = Some(location);
        self
    }

    /// Set the nearby peer count.
    pub fn with_nearby_peers(mut self, count: u32) -> Self {
        self.nearby_peer_count = count;
        self
    }

    /// Set the presence score.
    pub fn with_presence_score(mut self, score: u8) -> Self {
        self.presence_score = score.min(100);
        self
    }

    /// Add a sensor reading.
    pub fn with_sensor_reading(mut self, reading: SensorReading) -> Self {
        self.sensor_readings.push(reading);
        self
    }

    // ========================================================================
    // Host Function Implementations
    // ========================================================================

    // These methods implement the actual logic behind each host function.
    // In the wasmer integration, these are called from the imported functions.

    /// @location — Get the node's coarse GPS coordinates.
    ///
    /// Returns (latitude, longitude) rounded to ~1km precision.
    /// Returns None if location is unavailable or restricted by the user.
    pub fn get_location(&self) -> Option<(f32, f32)> {
        self.location.map(|loc| (loc.lat, loc.lon))
    }

    /// @proximity — Get the count of nearby Bluetooth/Wi-Fi peers.
    ///
    /// Returns the number of distinct peers detected in the last scan.
    /// WHY: Only count is exposed, not identities, to protect peer privacy.
    pub fn get_nearby_peers(&self) -> u32 {
        self.nearby_peer_count
    }

    /// @presence — Get the caller's Composite Presence Score.
    ///
    /// Returns a value from 40 (minimum, core sensors only) to 100 (all sensors
    /// + long participation history). This score affects block production
    /// selection probability but NOT mining rewards.
    pub fn get_presence_score(&self) -> u8 {
        self.presence_score
    }

    /// @sensor — Get the latest reading for a specific sensor type.
    ///
    /// Returns None if the sensor is not available on this device or if the
    /// user has not opted in (for enhanced sensors like camera/microphone).
    pub fn get_sensor_data(&self, sensor_type: SensorType) -> Option<&SensorReading> {
        self.sensor_readings
            .iter()
            .find(|r| r.sensor_type == sensor_type)
    }

    /// Get the current block height.
    pub fn get_block_height(&self) -> u64 {
        self.block_height
    }

    /// Get the current block timestamp (Unix seconds).
    pub fn get_block_timestamp(&self) -> u64 {
        self.block_timestamp
    }

    /// Get the address of the caller (transaction sender).
    pub fn get_caller_address(&self) -> Address {
        self.caller_address
    }

    /// Get the balance of the caller in Lux.
    pub fn get_caller_balance(&self) -> Lux {
        self.caller_balance
    }

    // ========================================================================
    // Contract Storage
    // ========================================================================

    /// Read a value from contract storage.
    pub fn storage_read(&self, key: &[u8; 32]) -> Option<&Vec<u8>> {
        self.storage.get(key)
    }

    /// Write a value to contract storage.
    pub fn storage_write(&mut self, key: [u8; 32], value: Vec<u8>) {
        self.storage.insert(key, value);
    }

    /// Delete a value from contract storage.
    pub fn storage_delete(&mut self, key: &[u8; 32]) -> bool {
        self.storage.remove(key).is_some()
    }

    /// Get all storage entries (for state commitment).
    pub fn storage_entries(&self) -> &std::collections::HashMap<[u8; 32], Vec<u8>> {
        &self.storage
    }

    // ========================================================================
    // Event Emission
    // ========================================================================

    /// Emit a contract event (log).
    pub fn emit_event(&mut self, contract_address: Address, topic: String, data: Vec<u8>) {
        self.events.push(ContractEvent {
            contract_address,
            topic,
            data,
        });
    }

    /// Drain all emitted events (consumed after execution).
    pub fn take_events(&mut self) -> Vec<ContractEvent> {
        std::mem::take(&mut self.events)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_env() -> HostEnvironment {
        let caller = Address([1u8; 32]);
        HostEnvironment::new(100, 1700000000, caller, 5_000_000)
            .with_location(GeoLocation {
                lat: 37.7749,
                lon: -122.4194,
            })
            .with_nearby_peers(12)
            .with_presence_score(75)
            .with_sensor_reading(SensorReading {
                sensor_type: SensorType::Barometer,
                value: 1013.25,
                timestamp_secs: 1700000000,
                is_fresh: true,
            })
    }

    #[test]
    fn test_get_location() {
        let env = make_test_env();
        let loc = env.get_location().unwrap();
        assert!((loc.0 - 37.7749).abs() < 0.001);
        assert!((loc.1 - (-122.4194)).abs() < 0.001);
    }

    #[test]
    fn test_get_location_unavailable() {
        let caller = Address([1u8; 32]);
        let env = HostEnvironment::new(100, 1700000000, caller, 0);
        assert!(env.get_location().is_none());
    }

    #[test]
    fn test_get_nearby_peers() {
        let env = make_test_env();
        assert_eq!(env.get_nearby_peers(), 12);
    }

    #[test]
    fn test_get_presence_score() {
        let env = make_test_env();
        assert_eq!(env.get_presence_score(), 75);
    }

    #[test]
    fn test_presence_score_capped_at_100() {
        let caller = Address([1u8; 32]);
        let env = HostEnvironment::new(1, 1, caller, 0).with_presence_score(200);
        assert_eq!(env.get_presence_score(), 100);
    }

    #[test]
    fn test_get_sensor_data() {
        let env = make_test_env();

        let reading = env.get_sensor_data(SensorType::Barometer);
        assert!(reading.is_some());
        assert!((reading.unwrap().value - 1013.25).abs() < 0.01);

        // Sensor not present on this device.
        let reading = env.get_sensor_data(SensorType::Gyroscope);
        assert!(reading.is_none());
    }

    #[test]
    fn test_block_context() {
        let env = make_test_env();
        assert_eq!(env.get_block_height(), 100);
        assert_eq!(env.get_block_timestamp(), 1700000000);
    }

    #[test]
    fn test_caller_context() {
        let env = make_test_env();
        assert_eq!(env.get_caller_address(), Address([1u8; 32]));
        assert_eq!(env.get_caller_balance(), 5_000_000);
    }

    #[test]
    fn test_storage_read_write() {
        let mut env = make_test_env();
        let key = [42u8; 32];
        let value = vec![1, 2, 3, 4];

        assert!(env.storage_read(&key).is_none());

        env.storage_write(key, value.clone());
        assert_eq!(env.storage_read(&key).unwrap(), &value);

        env.storage_delete(&key);
        assert!(env.storage_read(&key).is_none());
    }

    #[test]
    fn test_event_emission() {
        let mut env = make_test_env();
        let contract_addr = Address([2u8; 32]);

        env.emit_event(contract_addr, "Transfer".to_string(), vec![1, 2, 3]);
        env.emit_event(contract_addr, "Approval".to_string(), vec![4, 5]);

        assert_eq!(env.events.len(), 2);

        let events = env.take_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].topic, "Transfer");
        assert_eq!(events[1].topic, "Approval");

        // Events should be drained.
        assert!(env.events.is_empty());
    }
}
