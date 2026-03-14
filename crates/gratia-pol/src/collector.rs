//! Sensor data collector — receives events from the platform sensor layer.
//!
//! This module defines the interface between the native platform sensor layer
//! (Android Kotlin / iOS Swift) and the Rust PoL engine. The native layer
//! implements `SensorCollector` and pushes `SensorEvent`s through the UniFFI
//! bridge. `SensorEventBuffer` accumulates events throughout the day and
//! translates them into a `DailyProofOfLifeData` snapshot.
//!
//! PRIVACY: All sensor data stays on-device. The buffer is cleared daily
//! after the PoL attestation is generated.

use chrono::{DateTime, Utc};
use gratia_core::{
    DailyProofOfLifeData, GeoLocation, OptionalSensorData,
};
use std::collections::HashSet;

// ============================================================================
// Sensor Event Types
// ============================================================================

/// A single sensor event received from the platform layer.
#[derive(Debug, Clone)]
pub enum SensorEvent {
    /// Phone was unlocked by the user.
    Unlock {
        timestamp: DateTime<Utc>,
    },
    /// A screen interaction session was recorded.
    /// The native layer is responsible for sessionizing raw touch events;
    /// we only receive the session boundary.
    Interaction {
        timestamp: DateTime<Utc>,
        /// Duration of the interaction session in seconds.
        duration_secs: u32,
    },
    /// The phone's orientation changed (picked up, rotated, set down).
    OrientationChange {
        timestamp: DateTime<Utc>,
    },
    /// Accelerometer detected human-consistent motion.
    /// The native layer runs a lightweight classifier on raw accelerometer
    /// data and emits this event when motion passes the "human" threshold.
    Motion {
        timestamp: DateTime<Utc>,
    },
    /// A GPS fix was obtained.
    GpsUpdate {
        timestamp: DateTime<Utc>,
        lat: f32,
        lon: f32,
    },
    /// A Wi-Fi scan completed, reporting visible BSSIDs.
    WifiScan {
        timestamp: DateTime<Utc>,
        /// Opaque hashes of BSSIDs — we never store raw network identifiers.
        bssid_hashes: Vec<u64>,
    },
    /// A Bluetooth scan completed, reporting nearby peers.
    BluetoothScan {
        timestamp: DateTime<Utc>,
        /// Opaque hashes of peer device addresses.
        peer_hashes: Vec<u64>,
    },
    /// A charge state change occurred (plugged in or unplugged).
    ChargeEvent {
        timestamp: DateTime<Utc>,
        /// True if the phone just started charging, false if unplugged.
        is_charging: bool,
    },
}

// ============================================================================
// Sensor Collector Trait
// ============================================================================

/// Trait that the platform-native sensor layer must implement.
///
/// The Android `SensorManager` (Kotlin) and iOS sensor managers (Swift)
/// implement this trait via UniFFI bindings. The Rust side calls `poll()`
/// or the native side pushes events via `on_event()`.
pub trait SensorCollector: Send + Sync {
    /// Start sensor collection. Called once at app launch.
    fn start(&mut self) -> Result<(), String>;

    /// Stop sensor collection. Called when the app is shutting down.
    fn stop(&mut self) -> Result<(), String>;

    /// Check whether the collector is currently active.
    fn is_active(&self) -> bool;

    /// Drain all pending events from the native sensor queue.
    /// Returns events accumulated since the last call.
    fn drain_events(&mut self) -> Vec<SensorEvent>;
}

// ============================================================================
// Sensor Event Buffer
// ============================================================================

/// Accumulates sensor events throughout the day and maintains running
/// PoL parameter state. Feeds into `ProofOfLifeManager` at end-of-day.
pub struct SensorEventBuffer {
    /// Timestamps of all unlock events.
    unlock_timestamps: Vec<DateTime<Utc>>,

    /// Number of interaction sessions recorded.
    interaction_count: u32,

    /// Timestamps and durations of interaction sessions (for behavioral profiling).
    interaction_sessions: Vec<(DateTime<Utc>, u32)>,

    /// Whether at least one orientation change was detected.
    orientation_changed: bool,

    /// Whether human-consistent motion was detected.
    human_motion_detected: bool,

    /// Most recent GPS fix, if any.
    last_gps: Option<(f32, f32)>,

    /// Whether any GPS fix was obtained.
    gps_fix_obtained: bool,

    /// Set of unique Wi-Fi BSSID hashes seen today.
    wifi_bssids: HashSet<u64>,

    /// Distinct Bluetooth peer environment snapshots.
    /// Each entry is a sorted set of peer hashes from one scan.
    /// We count distinct environments by comparing consecutive scans.
    bt_environments: Vec<Vec<u64>>,

    /// Number of distinct BT environments detected.
    distinct_bt_environments: u32,

    /// Whether a charge cycle event occurred.
    charge_event_occurred: bool,

    /// Timestamps of charge events (for behavioral profiling).
    charge_timestamps: Vec<DateTime<Utc>>,

    /// Optional sensor data flags.
    optional_sensors: OptionalSensorData,
}

impl SensorEventBuffer {
    pub fn new() -> Self {
        SensorEventBuffer {
            unlock_timestamps: Vec::new(),
            interaction_count: 0,
            interaction_sessions: Vec::new(),
            orientation_changed: false,
            human_motion_detected: false,
            last_gps: None,
            gps_fix_obtained: false,
            wifi_bssids: HashSet::new(),
            bt_environments: Vec::new(),
            distinct_bt_environments: 0,
            charge_event_occurred: false,
            charge_timestamps: Vec::new(),
            optional_sensors: OptionalSensorData::default(),
        }
    }

    /// Process a single sensor event and update internal PoL state.
    pub fn process_event(&mut self, event: SensorEvent) {
        match event {
            SensorEvent::Unlock { timestamp } => {
                self.unlock_timestamps.push(timestamp);
            }
            SensorEvent::Interaction { timestamp, duration_secs } => {
                self.interaction_count += 1;
                self.interaction_sessions.push((timestamp, duration_secs));
            }
            SensorEvent::OrientationChange { .. } => {
                self.orientation_changed = true;
            }
            SensorEvent::Motion { .. } => {
                self.human_motion_detected = true;
            }
            SensorEvent::GpsUpdate { lat, lon, .. } => {
                self.gps_fix_obtained = true;
                self.last_gps = Some((lat, lon));
            }
            SensorEvent::WifiScan { bssid_hashes, .. } => {
                for hash in bssid_hashes {
                    self.wifi_bssids.insert(hash);
                }
            }
            SensorEvent::BluetoothScan { peer_hashes, .. } => {
                let mut sorted = peer_hashes.clone();
                sorted.sort();

                // WHY: We count a "distinct environment" when the set of visible
                // Bluetooth peers differs from the most recently recorded environment.
                // This detects that the phone has moved to a different physical
                // location (different nearby devices). A phone farm will see the
                // same peers every scan.
                let is_new_environment = match self.bt_environments.last() {
                    Some(prev) => *prev != sorted,
                    None => true,
                };

                if is_new_environment {
                    self.distinct_bt_environments += 1;
                    self.bt_environments.push(sorted);
                }
            }
            SensorEvent::ChargeEvent { timestamp, .. } => {
                self.charge_event_occurred = true;
                self.charge_timestamps.push(timestamp);
            }
        }
    }

    /// Process a batch of events.
    pub fn process_events(&mut self, events: Vec<SensorEvent>) {
        for event in events {
            self.process_event(event);
        }
    }

    /// Build a `DailyProofOfLifeData` snapshot from the accumulated events.
    ///
    /// This is called at end-of-day to generate the day's PoL attestation input.
    pub fn to_daily_data(&self) -> DailyProofOfLifeData {
        let first_unlock = self.unlock_timestamps.iter().min().copied();
        let last_unlock = self.unlock_timestamps.iter().max().copied();

        let approximate_location = self.last_gps.map(|(lat, lon)| GeoLocation { lat, lon });

        DailyProofOfLifeData {
            unlock_count: self.unlock_timestamps.len() as u32,
            first_unlock,
            last_unlock,
            interaction_sessions: self.interaction_count,
            orientation_changed: self.orientation_changed,
            human_motion_detected: self.human_motion_detected,
            gps_fix_obtained: self.gps_fix_obtained,
            approximate_location,
            distinct_wifi_networks: self.wifi_bssids.len() as u32,
            distinct_bt_environments: self.distinct_bt_environments,
            charge_cycle_event: self.charge_event_occurred,
            optional_sensors: self.optional_sensors.clone(),
        }
    }

    /// Reset the buffer for a new day. Called after `to_daily_data()`.
    pub fn reset(&mut self) {
        self.unlock_timestamps.clear();
        self.interaction_count = 0;
        self.interaction_sessions.clear();
        self.orientation_changed = false;
        self.human_motion_detected = false;
        self.last_gps = None;
        self.gps_fix_obtained = false;
        self.wifi_bssids.clear();
        self.bt_environments.clear();
        self.distinct_bt_environments = 0;
        self.charge_event_occurred = false;
        self.charge_timestamps.clear();
        self.optional_sensors = OptionalSensorData::default();
    }

    /// Update optional sensor flags (called by the native layer when optional
    /// sensors become available or produce data).
    pub fn set_optional_sensors(&mut self, sensors: OptionalSensorData) {
        self.optional_sensors = sensors;
    }

    // --- Accessors for behavioral profiling ---

    /// Get unlock timestamps for behavioral analysis.
    pub fn unlock_timestamps(&self) -> &[DateTime<Utc>] {
        &self.unlock_timestamps
    }

    /// Get interaction sessions (timestamp, duration) for behavioral analysis.
    pub fn interaction_sessions(&self) -> &[(DateTime<Utc>, u32)] {
        &self.interaction_sessions
    }

    /// Get charge event timestamps for behavioral analysis.
    pub fn charge_timestamps(&self) -> &[DateTime<Utc>] {
        &self.charge_timestamps
    }

    /// Get the number of distinct Bluetooth environments.
    pub fn distinct_bt_environments(&self) -> u32 {
        self.distinct_bt_environments
    }
}

impl Default for SensorEventBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn ts(hours_ago: i64) -> DateTime<Utc> {
        Utc::now() - Duration::hours(hours_ago)
    }

    #[test]
    fn test_empty_buffer_produces_invalid_data() {
        let buffer = SensorEventBuffer::new();
        let data = buffer.to_daily_data();
        assert!(!data.is_valid());
        assert_eq!(data.unlock_count, 0);
    }

    #[test]
    fn test_unlock_events_tracked() {
        let mut buffer = SensorEventBuffer::new();
        for i in 0..12 {
            buffer.process_event(SensorEvent::Unlock {
                timestamp: ts(12 - i),
            });
        }
        let data = buffer.to_daily_data();
        assert_eq!(data.unlock_count, 12);
        assert!(data.first_unlock.is_some());
        assert!(data.last_unlock.is_some());
    }

    #[test]
    fn test_unlock_spread_computed_correctly() {
        let mut buffer = SensorEventBuffer::new();
        buffer.process_event(SensorEvent::Unlock { timestamp: ts(10) });
        buffer.process_event(SensorEvent::Unlock { timestamp: ts(1) });
        let data = buffer.to_daily_data();
        // Spread should be ~9 hours
        let spread = (data.last_unlock.unwrap() - data.first_unlock.unwrap()).num_hours();
        assert!(spread >= 8); // Allow small rounding
    }

    #[test]
    fn test_interaction_sessions_counted() {
        let mut buffer = SensorEventBuffer::new();
        buffer.process_event(SensorEvent::Interaction {
            timestamp: ts(5),
            duration_secs: 120,
        });
        buffer.process_event(SensorEvent::Interaction {
            timestamp: ts(3),
            duration_secs: 60,
        });
        let data = buffer.to_daily_data();
        assert_eq!(data.interaction_sessions, 2);
    }

    #[test]
    fn test_orientation_and_motion_flags() {
        let mut buffer = SensorEventBuffer::new();
        assert!(!buffer.to_daily_data().orientation_changed);
        assert!(!buffer.to_daily_data().human_motion_detected);

        buffer.process_event(SensorEvent::OrientationChange { timestamp: ts(2) });
        buffer.process_event(SensorEvent::Motion { timestamp: ts(1) });

        let data = buffer.to_daily_data();
        assert!(data.orientation_changed);
        assert!(data.human_motion_detected);
    }

    #[test]
    fn test_gps_fix_recorded() {
        let mut buffer = SensorEventBuffer::new();
        buffer.process_event(SensorEvent::GpsUpdate {
            timestamp: ts(3),
            lat: 40.7128,
            lon: -74.0060,
        });
        let data = buffer.to_daily_data();
        assert!(data.gps_fix_obtained);
        let loc = data.approximate_location.unwrap();
        assert!((loc.lat - 40.7128).abs() < 0.001);
        assert!((loc.lon - (-74.0060)).abs() < 0.001);
    }

    #[test]
    fn test_wifi_deduplication() {
        let mut buffer = SensorEventBuffer::new();
        // Two scans with overlapping BSSIDs
        buffer.process_event(SensorEvent::WifiScan {
            timestamp: ts(5),
            bssid_hashes: vec![100, 200, 300],
        });
        buffer.process_event(SensorEvent::WifiScan {
            timestamp: ts(3),
            bssid_hashes: vec![200, 300, 400],
        });
        let data = buffer.to_daily_data();
        assert_eq!(data.distinct_wifi_networks, 4); // 100, 200, 300, 400
    }

    #[test]
    fn test_bluetooth_environment_detection() {
        let mut buffer = SensorEventBuffer::new();

        // First environment
        buffer.process_event(SensorEvent::BluetoothScan {
            timestamp: ts(8),
            peer_hashes: vec![1, 2, 3],
        });
        // Same environment again (same peers)
        buffer.process_event(SensorEvent::BluetoothScan {
            timestamp: ts(6),
            peer_hashes: vec![3, 2, 1], // Same set, different order
        });
        // New environment (different peers)
        buffer.process_event(SensorEvent::BluetoothScan {
            timestamp: ts(3),
            peer_hashes: vec![10, 20, 30],
        });

        let data = buffer.to_daily_data();
        assert_eq!(data.distinct_bt_environments, 2);
    }

    #[test]
    fn test_charge_event_tracked() {
        let mut buffer = SensorEventBuffer::new();
        assert!(!buffer.to_daily_data().charge_cycle_event);

        buffer.process_event(SensorEvent::ChargeEvent {
            timestamp: ts(4),
            is_charging: true,
        });

        let data = buffer.to_daily_data();
        assert!(data.charge_cycle_event);
    }

    #[test]
    fn test_reset_clears_all_state() {
        let mut buffer = SensorEventBuffer::new();
        buffer.process_event(SensorEvent::Unlock { timestamp: ts(5) });
        buffer.process_event(SensorEvent::OrientationChange { timestamp: ts(4) });
        buffer.process_event(SensorEvent::GpsUpdate {
            timestamp: ts(3),
            lat: 0.0,
            lon: 0.0,
        });
        buffer.process_event(SensorEvent::ChargeEvent {
            timestamp: ts(2),
            is_charging: true,
        });

        buffer.reset();
        let data = buffer.to_daily_data();
        assert_eq!(data.unlock_count, 0);
        assert!(!data.orientation_changed);
        assert!(!data.gps_fix_obtained);
        assert!(!data.charge_cycle_event);
    }

    #[test]
    fn test_process_events_batch() {
        let mut buffer = SensorEventBuffer::new();
        let events = vec![
            SensorEvent::Unlock { timestamp: ts(10) },
            SensorEvent::Unlock { timestamp: ts(5) },
            SensorEvent::Motion { timestamp: ts(3) },
        ];
        buffer.process_events(events);
        let data = buffer.to_daily_data();
        assert_eq!(data.unlock_count, 2);
        assert!(data.human_motion_detected);
    }

    #[test]
    fn test_full_valid_day_through_buffer() {
        let mut buffer = SensorEventBuffer::new();
        let now = Utc::now();

        // Simulate a realistic day of phone usage
        for i in 0..15 {
            buffer.process_event(SensorEvent::Unlock {
                timestamp: now - Duration::hours(14) + Duration::hours(i),
            });
        }
        for i in 0..8 {
            buffer.process_event(SensorEvent::Interaction {
                timestamp: now - Duration::hours(12) + Duration::hours(i * 2),
                duration_secs: 60 + (i as u32 * 30),
            });
        }
        buffer.process_event(SensorEvent::OrientationChange { timestamp: now - Duration::hours(10) });
        buffer.process_event(SensorEvent::Motion { timestamp: now - Duration::hours(8) });
        buffer.process_event(SensorEvent::GpsUpdate {
            timestamp: now - Duration::hours(6),
            lat: 51.5074,
            lon: -0.1278,
        });
        buffer.process_event(SensorEvent::WifiScan {
            timestamp: now - Duration::hours(5),
            bssid_hashes: vec![111, 222],
        });
        buffer.process_event(SensorEvent::BluetoothScan {
            timestamp: now - Duration::hours(7),
            peer_hashes: vec![10, 20],
        });
        buffer.process_event(SensorEvent::BluetoothScan {
            timestamp: now - Duration::hours(2),
            peer_hashes: vec![30, 40],
        });
        buffer.process_event(SensorEvent::ChargeEvent {
            timestamp: now - Duration::hours(1),
            is_charging: true,
        });

        let data = buffer.to_daily_data();
        assert!(data.is_valid());
    }
}
