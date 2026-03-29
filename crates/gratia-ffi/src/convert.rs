//! Conversion functions between FFI-safe types and internal Rust types.
//!
//! UniFFI requires types to be simple, self-contained, and free of generics
//! or complex trait bounds. This module bridges the gap between the rich
//! internal types and the flat FFI representations.

use chrono::Utc;

use gratia_core::types::{Address, MiningState, StakeInfo};
use gratia_pol::collector::SensorEvent;
use gratia_wallet::{TransactionDirection, TransactionRecord, TransactionStatus};

use crate::{FfiSensorEvent, FfiStakeInfo, FfiTransactionInfo};

// ============================================================================
// Address <-> hex string
// ============================================================================

/// Parse a hex address string (with or without "grat:" prefix) into an Address.
pub fn address_from_hex(s: &str) -> Result<Address, String> {
    let hex_str = s.strip_prefix("grat:").unwrap_or(s);
    let bytes = hex::decode(hex_str).map_err(|e| format!("invalid hex address: {}", e))?;
    if bytes.len() != 32 {
        return Err(format!(
            "address must be 32 bytes, got {}",
            bytes.len()
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Address(arr))
}

/// Convert an Address to its display string ("grat:<hex>").
pub fn address_to_hex(addr: &Address) -> String {
    format!("grat:{}", hex::encode(addr.0))
}

// ============================================================================
// MiningState <-> String
// ============================================================================

/// Convert a MiningState enum to a human-readable string.
pub fn mining_state_to_string(state: &MiningState) -> String {
    match state {
        MiningState::ProofOfLife => "proof_of_life".to_string(),
        MiningState::PendingActivation => "pending_activation".to_string(),
        MiningState::Mining => "mining".to_string(),
        MiningState::Throttled => "throttled".to_string(),
        MiningState::BatteryLow => "battery_low".to_string(),
    }
}

// ============================================================================
// FfiSensorEvent -> SensorEvent
// ============================================================================

impl From<FfiSensorEvent> for SensorEvent {
    fn from(ffi: FfiSensorEvent) -> Self {
        // WHY: We use Utc::now() as the timestamp for all FFI events because
        // the native layer calls us in real-time as events happen. The timestamp
        // represents "when the Rust layer received the event," which is close
        // enough to the actual event time for PoL purposes.
        let now = Utc::now();

        match ffi {
            FfiSensorEvent::Unlock => SensorEvent::Unlock { timestamp: now },
            FfiSensorEvent::Interaction { duration_secs } => SensorEvent::Interaction {
                timestamp: now,
                duration_secs,
            },
            FfiSensorEvent::OrientationChange => {
                SensorEvent::OrientationChange { timestamp: now }
            }
            FfiSensorEvent::Motion => SensorEvent::Motion { timestamp: now },
            FfiSensorEvent::GpsUpdate { lat, lon } => SensorEvent::GpsUpdate {
                timestamp: now,
                lat,
                lon,
            },
            FfiSensorEvent::WifiScan { bssid_hashes } => SensorEvent::WifiScan {
                timestamp: now,
                bssid_hashes,
            },
            FfiSensorEvent::BluetoothScan { peer_hashes } => SensorEvent::BluetoothScan {
                timestamp: now,
                peer_hashes,
            },
            FfiSensorEvent::ChargeEvent { is_charging } => SensorEvent::ChargeEvent {
                timestamp: now,
                is_charging,
            },
            // WHY: Environmental sensor readings (barometer, light, magnetometer,
            // accelerometer) are cached in GratiaNodeInner for VM host functions
            // but don't map to PoL sensor events. The PoL engine cares about
            // motion patterns, not raw sensor values. We map these to Motion
            // events with zero impact on PoL validation (motion requires threshold
            // detection, which these won't trigger).
            FfiSensorEvent::BarometerReading { .. }
            | FfiSensorEvent::LightReading { .. }
            | FfiSensorEvent::MagnetometerReading { .. }
            | FfiSensorEvent::AccelerometerReading { .. } => {
                // No-op for PoL: these are only used by the VM sensor cache
                SensorEvent::Motion { timestamp: now }
            }
        }
    }
}

// ============================================================================
// TransactionRecord -> FfiTransactionInfo
// ============================================================================

impl From<&TransactionRecord> for FfiTransactionInfo {
    fn from(rec: &TransactionRecord) -> Self {
        let direction = match rec.direction {
            TransactionDirection::Sent => "sent".to_string(),
            TransactionDirection::Received => "received".to_string(),
        };
        let status = match rec.status {
            TransactionStatus::Pending => "pending".to_string(),
            TransactionStatus::Confirmed => "confirmed".to_string(),
            TransactionStatus::Failed => "failed".to_string(),
        };
        FfiTransactionInfo {
            hash_hex: rec.hash.clone(),
            direction,
            counterparty: rec.counterparty.map(|a| address_to_hex(&a)),
            amount_lux: rec.amount,
            timestamp_millis: rec.timestamp.timestamp_millis(),
            status,
        }
    }
}

// ============================================================================
// StakeInfo -> FfiStakeInfo
// ============================================================================

impl From<&StakeInfo> for FfiStakeInfo {
    fn from(info: &StakeInfo) -> Self {
        FfiStakeInfo {
            node_stake_lux: info.node_stake,
            overflow_amount_lux: info.overflow_amount,
            total_committed_lux: info.total_committed,
            staked_at_millis: info.staked_at.timestamp_millis(),
            meets_minimum: info.meets_minimum,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gratia_core::types::Address;

    #[test]
    fn test_address_roundtrip() {
        let addr = Address([0xABu8; 32]);
        let hex_str = address_to_hex(&addr);
        assert!(hex_str.starts_with("grat:"));

        let parsed = address_from_hex(&hex_str).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn test_address_from_hex_without_prefix() {
        let addr = Address([0x42u8; 32]);
        let raw_hex = hex::encode(addr.0);
        let parsed = address_from_hex(&raw_hex).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn test_address_from_hex_invalid_length() {
        let result = address_from_hex("aabb");
        assert!(result.is_err());
    }

    #[test]
    fn test_address_from_hex_invalid_chars() {
        let result = address_from_hex("zzzz");
        assert!(result.is_err());
    }

    #[test]
    fn test_mining_state_strings() {
        assert_eq!(
            mining_state_to_string(&MiningState::ProofOfLife),
            "proof_of_life"
        );
        assert_eq!(
            mining_state_to_string(&MiningState::Mining),
            "mining"
        );
        assert_eq!(
            mining_state_to_string(&MiningState::Throttled),
            "throttled"
        );
        assert_eq!(
            mining_state_to_string(&MiningState::BatteryLow),
            "battery_low"
        );
        assert_eq!(
            mining_state_to_string(&MiningState::PendingActivation),
            "pending_activation"
        );
    }

    #[test]
    fn test_ffi_sensor_event_unlock_converts() {
        let ffi = FfiSensorEvent::Unlock;
        let event: SensorEvent = ffi.into();
        match event {
            SensorEvent::Unlock { timestamp } => {
                // Timestamp should be very recent (within last second)
                let elapsed = Utc::now() - timestamp;
                assert!(elapsed.num_seconds() < 2);
            }
            _ => panic!("expected Unlock variant"),
        }
    }

    #[test]
    fn test_ffi_sensor_event_gps_converts() {
        let ffi = FfiSensorEvent::GpsUpdate {
            lat: 40.7128,
            lon: -74.006,
        };
        let event: SensorEvent = ffi.into();
        match event {
            SensorEvent::GpsUpdate { lat, lon, .. } => {
                assert!((lat - 40.7128).abs() < 0.0001);
                assert!((lon - (-74.006)).abs() < 0.0001);
            }
            _ => panic!("expected GpsUpdate variant"),
        }
    }

    #[test]
    fn test_ffi_sensor_event_wifi_scan_converts() {
        let ffi = FfiSensorEvent::WifiScan {
            bssid_hashes: vec![100, 200, 300],
        };
        let event: SensorEvent = ffi.into();
        match event {
            SensorEvent::WifiScan { bssid_hashes, .. } => {
                assert_eq!(bssid_hashes, vec![100, 200, 300]);
            }
            _ => panic!("expected WifiScan variant"),
        }
    }

    #[test]
    fn test_ffi_sensor_event_bluetooth_scan_converts() {
        let ffi = FfiSensorEvent::BluetoothScan {
            peer_hashes: vec![10, 20],
        };
        let event: SensorEvent = ffi.into();
        match event {
            SensorEvent::BluetoothScan { peer_hashes, .. } => {
                assert_eq!(peer_hashes, vec![10, 20]);
            }
            _ => panic!("expected BluetoothScan variant"),
        }
    }

    #[test]
    fn test_ffi_sensor_event_charge_converts() {
        let ffi = FfiSensorEvent::ChargeEvent { is_charging: true };
        let event: SensorEvent = ffi.into();
        match event {
            SensorEvent::ChargeEvent { is_charging, .. } => {
                assert!(is_charging);
            }
            _ => panic!("expected ChargeEvent variant"),
        }
    }

    #[test]
    fn test_ffi_sensor_event_interaction_converts() {
        let ffi = FfiSensorEvent::Interaction { duration_secs: 120 };
        let event: SensorEvent = ffi.into();
        match event {
            SensorEvent::Interaction { duration_secs, .. } => {
                assert_eq!(duration_secs, 120);
            }
            _ => panic!("expected Interaction variant"),
        }
    }

    #[test]
    fn test_transaction_record_to_ffi() {
        let rec = TransactionRecord {
            hash: "abc123".to_string(),
            direction: TransactionDirection::Sent,
            amount: 5_000_000,
            counterparty: Some(Address([0x11u8; 32])),
            timestamp: Utc::now(),
            status: TransactionStatus::Confirmed,
        };
        let ffi: FfiTransactionInfo = (&rec).into();
        assert_eq!(ffi.hash_hex, "abc123");
        assert_eq!(ffi.direction, "sent");
        assert_eq!(ffi.status, "confirmed");
        assert_eq!(ffi.amount_lux, 5_000_000);
        assert!(ffi.counterparty.is_some());
    }

    #[test]
    fn test_stake_info_to_ffi() {
        let info = StakeInfo {
            node_stake: 1_000_000,
            overflow_amount: 500_000,
            total_committed: 1_500_000,
            staked_at: Utc::now(),
            meets_minimum: true,
        };
        let ffi: FfiStakeInfo = (&info).into();
        assert_eq!(ffi.node_stake_lux, 1_000_000);
        assert_eq!(ffi.overflow_amount_lux, 500_000);
        assert_eq!(ffi.total_committed_lux, 1_500_000);
        assert!(ffi.meets_minimum);
    }
}
