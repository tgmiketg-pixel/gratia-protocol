//! Phone Farm Attack Simulation Tests
//!
//! Simulates an attacker controlling multiple phones in a farm. The Gratia
//! protocol's Proof of Life system should detect phone farms through:
//! - Identical timing patterns across devices
//! - Non-organic interaction patterns (scripted touch events)
//! - Identical sensor data (accelerometer, GPS, BT peers) across phones
//! - Overlapping Bluetooth peer hashes (co-location signal)
//! - Mismatched GPS diversity vs. BT peer overlap
//!
//! A legitimate single-phone user should pass all checks cleanly.

use chrono::{Duration, TimeZone, Utc};
use gratia_core::config::ProofOfLifeConfig;
use gratia_core::{DailyProofOfLifeData, GeoLocation, OptionalSensorData};
use gratia_pol::behavioral_anomaly::{AnomalyFlag, BehavioralFingerprint, DailyBehavioralSummary};
use gratia_pol::clustering::{
    ClusterDetector, ClusterType, NodeDailyReport, PeerSetHash,
};
use gratia_pol::validator::PolValidator;
use gratia_core::types::NodeId;

// ============================================================================
// Helpers
// ============================================================================

fn test_node(id: u8) -> NodeId {
    let mut bytes = [0u8; 32];
    bytes[0] = id;
    NodeId(bytes)
}

/// Build a valid DailyProofOfLifeData that passes all 8 PoL parameters.
fn valid_day_data() -> DailyProofOfLifeData {
    let now = Utc::now();
    DailyProofOfLifeData {
        unlock_count: 25,
        first_unlock: Some(now - Duration::hours(12)),
        last_unlock: Some(now),
        interaction_sessions: 10,
        orientation_changed: true,
        human_motion_detected: true,
        gps_fix_obtained: true,
        approximate_location: Some(GeoLocation { lat: 40.712, lon: -74.006 }),
        distinct_wifi_networks: 3,
        distinct_bt_environments: 4,
        charge_cycle_event: true,
        optional_sensors: OptionalSensorData::default(),
    }
}

/// Build a realistic daily behavioral summary for a genuine human user.
fn realistic_human_day(day_offset: u32) -> DailyBehavioralSummary {
    // WHY: Deterministic pseudo-random jitter per day and metric to produce
    // varied but consistent human-like patterns across days.
    fn hash32(input: u32) -> u32 {
        let mut x = input;
        x = x.wrapping_add(0x9e3779b9);
        x ^= x >> 16;
        x = x.wrapping_mul(0x45d9f3b);
        x ^= x >> 16;
        x
    }

    fn jitter(day: u32, metric: u32) -> f32 {
        let seed = day.wrapping_mul(31).wrapping_add(metric).wrapping_mul(17);
        let h = hash32(seed);
        (h as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    let d = day_offset;
    DailyBehavioralSummary {
        date: Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
            + Duration::days(day_offset as i64),
        avg_unlock_hour: 10.5 + jitter(d, 0) * 1.5,
        unlock_count: (32.0 + jitter(d, 1) * 12.0).max(15.0) as u32,
        unlock_hour_spread: 5.0 + jitter(d, 2) * 1.5,
        total_screen_on_minutes: (170.0 + jitter(d, 3) * 70.0).max(60.0) as u32,
        movement_distance_meters: 6000.0 + (jitter(d, 4) as f64) * 4000.0,
        bluetooth_peer_diversity: (11.0 + jitter(d, 5) * 7.0).max(3.0) as u32,
        charge_start_hour: 23.0 + jitter(d, 6) * 2.0,
        interaction_richness: (0.7 + jitter(d, 7) * 0.2).clamp(0.3, 1.0),
    }
}

/// Build a phone farm day with identical timing and low interaction richness.
fn phone_farm_day(day_offset: u32) -> DailyBehavioralSummary {
    DailyBehavioralSummary {
        date: Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
            + Duration::days(day_offset as i64),
        // WHY: Phone farms use scripted unlock patterns at fixed times.
        avg_unlock_hour: 10.0,
        unlock_count: 10,
        unlock_hour_spread: 3.0,
        total_screen_on_minutes: 60,
        // WHY: Farm phones don't move — they sit on a shelf.
        movement_distance_meters: 50.0,
        // WHY: Farm phones all see the same BT peers.
        bluetooth_peer_diversity: 2,
        charge_start_hour: 22.0,
        // WHY: Scripted taps have very low interaction richness — no organic
        // swipes, varied touch positions, or natural pauses.
        interaction_richness: 0.08,
    }
}

fn make_node_report(
    node_byte: u8,
    day: u32,
    peer_hash: [u8; 32],
    mining_hour: f32,
) -> NodeDailyReport {
    NodeDailyReport {
        node_id: test_node(node_byte),
        date: Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
            + Duration::days(day as i64),
        peer_set_hash: PeerSetHash(peer_hash),
        charge_start_hour: 22.0,
        charge_end_hour: 6.0,
        mining_start_hour: mining_hour,
        mining_duration_minutes: 240,
        unlock_pattern_hash: [node_byte; 16],
    }
}

// ============================================================================
// Tests
// ============================================================================

/// ATTACK: Attacker generates synthetic unlock events on 10 phones simultaneously.
/// DEFENSE: PoL behavioral anomaly detection flags identical timing patterns.
#[test]
fn test_phone_farm_identical_timing_patterns_detected() {
    // Simulate 10 farm phones all submitting identical daily behavioral data.
    for phone_id in 0..10u8 {
        let mut fingerprint = BehavioralFingerprint::new();

        // All phones get the exact same behavioral pattern every day.
        for day in 0..15 {
            fingerprint.add_day(phone_farm_day(day));
        }

        let anomalies = fingerprint.detect_anomalies();

        // WHY: Identical patterns across days should trigger replay detection
        // because the day-to-day correlation exceeds 0.99.
        assert!(
            anomalies.contains(&AnomalyFlag::ReplayDetected)
                || anomalies.contains(&AnomalyFlag::StaticDevice)
                || anomalies.contains(&AnomalyFlag::LowInteraction),
            "Phone farm device {} should be flagged, got: {:?}",
            phone_id,
            anomalies
        );

        // Consistency score should be low due to static/replay patterns.
        assert!(
            fingerprint.score() < 50,
            "Phone farm device {} should have low score, got: {}",
            phone_id,
            fingerprint.score()
        );
    }
}

/// ATTACK: Attacker uses scripted touch events with low interaction richness.
/// DEFENSE: PoL flags low interaction richness (high unlocks, minimal touch diversity).
#[test]
fn test_scripted_touch_events_low_interaction_detected() {
    let mut fingerprint = BehavioralFingerprint::new();

    for day in 0..15 {
        let mut summary = realistic_human_day(day);
        // Override with scripted behavior: many unlocks but nearly zero richness.
        summary.unlock_count = 50;
        summary.interaction_richness = 0.05;
        fingerprint.add_day(summary);
    }

    let anomalies = fingerprint.detect_anomalies();

    assert!(
        anomalies.contains(&AnomalyFlag::LowInteraction),
        "Scripted touch events should trigger LowInteraction, got: {:?}",
        anomalies
    );
}

/// ATTACK: Attacker copies sensor data between phones (identical accelerometer/GPS/BT).
/// DEFENSE: Behavioral fingerprint detects replay (identical data across days = correlation > 0.99).
#[test]
fn test_copied_sensor_data_replay_detected() {
    let mut fingerprint = BehavioralFingerprint::new();
    let base_day = realistic_human_day(0);

    // Replay the same day's data across 10 days.
    for day in 0..10 {
        let mut copy = base_day.clone();
        copy.date = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
            + Duration::days(day);
        fingerprint.add_day(copy);
    }

    let anomalies = fingerprint.detect_anomalies();

    assert!(
        anomalies.contains(&AnomalyFlag::ReplayDetected),
        "Copied sensor data should trigger ReplayDetected, got: {:?}",
        anomalies
    );

    // Score should be penalized heavily.
    assert!(
        fingerprint.score() < 60,
        "Replay attack should produce low score, got: {}",
        fingerprint.score()
    );
}

/// ATTACK: All phones in same location — Bluetooth peer hashes overlap significantly.
/// DEFENSE: Cluster detector identifies co-located nodes with matching peer set hashes.
#[test]
fn test_colocated_phones_bt_peer_overlap_detected() {
    let detector = ClusterDetector::new();

    // All 10 farm phones see the same BT peers every day (same hash).
    let shared_hash = [0xAA; 32];
    let mut reports: Vec<Vec<NodeDailyReport>> = Vec::new();

    for phone in 0..10u8 {
        let phone_reports: Vec<NodeDailyReport> = (0..30)
            .map(|day| make_node_report(phone, day, shared_hash, 22.0))
            .collect();
        reports.push(phone_reports);
    }

    let alerts = detector.analyze_reports(&reports);

    // WHY: 30 days of matching peer hashes should produce many cluster alerts
    // for every pair of phones.
    assert!(
        !alerts.is_empty(),
        "Co-located phones with identical BT peers should be detected"
    );

    // All alerts should be FarmSignature or CoLocatedPeers.
    for alert in &alerts {
        assert!(
            alert.cluster_type == ClusterType::FarmSignature
                || alert.cluster_type == ClusterType::CoLocatedPeers,
            "Expected farm/co-located cluster type, got: {:?}",
            alert.cluster_type
        );
        assert!(
            alert.confidence > 0.5,
            "Cluster confidence should be high, got: {}",
            alert.confidence
        );
    }
}

/// ATTACK: Attacker varies fake GPS across phones but all phones see same BT peers.
/// DEFENSE: Cluster detector catches the BT peer overlap despite diverse GPS.
#[test]
fn test_varied_gps_same_bt_peers_caught() {
    let detector = ClusterDetector::new();

    // All phones report the same BT peers (co-located farm) but claim
    // different GPS locations to try to appear distributed.
    let shared_hash = [0xBB; 32];
    let mut reports: Vec<Vec<NodeDailyReport>> = Vec::new();

    for phone in 0..5u8 {
        let phone_reports: Vec<NodeDailyReport> = (0..20)
            .map(|day| {
                let mut report = make_node_report(phone, day, shared_hash, 22.0 + phone as f32 * 0.1);
                // Different unlock pattern hashes to simulate "different GPS"
                report.unlock_pattern_hash = [phone; 16];
                report
            })
            .collect();
        reports.push(phone_reports);
    }

    let alerts = detector.analyze_reports(&reports);

    // WHY: Identical BT peer hashes betray the co-location even though
    // GPS (unlock_pattern_hash proxy) differs. The cluster detector only
    // needs BT overlap + mining synchronization to flag.
    assert!(
        !alerts.is_empty(),
        "Same BT peers with varied GPS should still be caught"
    );

    for alert in &alerts {
        assert!(
            alert.shared_peer_days >= 14,
            "Expected >= 14 shared peer days, got: {}",
            alert.shared_peer_days
        );
    }
}

/// CONTROL: Legitimate user with 1 phone passes all checks correctly.
#[test]
fn test_legitimate_single_user_passes_all_checks() {
    // --- PoL parameter validation ---
    let validator = PolValidator::new(ProofOfLifeConfig::default());
    let data = valid_day_data();
    let result = validator.validate_daily_data(&data);

    assert!(result.passed, "Legitimate user should pass PoL validation");
    assert!(
        !result.suspicious_patterns.any_flagged(),
        "Legitimate user should have no suspicious patterns"
    );

    // --- Behavioral fingerprint ---
    let mut fingerprint = BehavioralFingerprint::new();
    for day in 0..30 {
        fingerprint.add_day(realistic_human_day(day));
    }

    let anomalies = fingerprint.detect_anomalies();
    assert!(
        anomalies.is_empty(),
        "Legitimate user should trigger no anomalies, got: {:?}",
        anomalies
    );

    assert!(
        fingerprint.score() > 70,
        "Legitimate user should have high behavioral score, got: {}",
        fingerprint.score()
    );
}

/// ATTACK: Farm operator uses exactly-minimum PoL parameters to pass validation.
/// DEFENSE: Suspicious patterns heuristic flags exact-minimum data.
#[test]
fn test_exact_minimum_pol_flagged_suspicious() {
    let validator = PolValidator::new(ProofOfLifeConfig::default());
    let now = Utc::now();

    // Craft data that exactly meets every minimum — classic farm signature.
    let data = DailyProofOfLifeData {
        unlock_count: 10,               // Exactly minimum
        first_unlock: Some(now - Duration::hours(6)), // Exactly 6-hour spread
        last_unlock: Some(now),
        interaction_sessions: 3,         // Exactly minimum
        orientation_changed: true,
        human_motion_detected: true,
        gps_fix_obtained: true,
        approximate_location: None,
        distinct_wifi_networks: 1,       // Bare minimum
        distinct_bt_environments: 2,     // Bare minimum
        charge_cycle_event: true,
        optional_sensors: OptionalSensorData::default(),
    };

    let result = validator.validate_daily_data(&data);

    // Passes validation (it meets all requirements)...
    assert!(result.passed, "Exact-minimum data should technically pass");

    // ...but is flagged as suspicious.
    assert!(
        result.suspicious_patterns.any_flagged(),
        "Exact-minimum data should be flagged as suspicious"
    );
    assert!(
        result.suspicious_patterns.suspicion_score > 0.0,
        "Suspicion score should be non-zero"
    );
}

/// ATTACK: Farm phones have zero movement (static devices on a shelf).
/// DEFENSE: Behavioral fingerprint flags static device.
#[test]
fn test_static_farm_device_flagged() {
    let mut fingerprint = BehavioralFingerprint::new();

    for day in 0..15 {
        let mut summary = realistic_human_day(day);
        // Override movement to near-zero — device sitting on a shelf.
        summary.movement_distance_meters = 20.0;
        fingerprint.add_day(summary);
    }

    let anomalies = fingerprint.detect_anomalies();

    assert!(
        anomalies.contains(&AnomalyFlag::StaticDevice),
        "Static device should be flagged, got: {:?}",
        anomalies
    );
}

/// ATTACK: Farm phones start mining at the same time (synchronized plugging in).
/// DEFENSE: Cluster detector identifies synchronized mining patterns.
#[test]
fn test_synchronized_mining_detected() {
    let detector = ClusterDetector::new();

    // 5 phones all start mining within the same hour AND share peers.
    let shared_hash = [0xCC; 32];
    let mut reports: Vec<Vec<NodeDailyReport>> = Vec::new();

    for phone in 0..5u8 {
        let phone_reports: Vec<NodeDailyReport> = (0..20)
            .map(|day| make_node_report(phone, day, shared_hash, 22.0))
            .collect();
        reports.push(phone_reports);
    }

    let alerts = detector.analyze_reports(&reports);

    assert!(
        !alerts.is_empty(),
        "Synchronized mining should be detected"
    );

    // At least some alerts should be FarmSignature (both co-located and synchronized).
    let farm_alerts: Vec<_> = alerts
        .iter()
        .filter(|a| a.cluster_type == ClusterType::FarmSignature)
        .collect();
    assert!(
        !farm_alerts.is_empty(),
        "Should detect FarmSignature cluster type"
    );
}
