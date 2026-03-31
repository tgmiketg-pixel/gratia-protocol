//! Behavioral Spoofing Tests
//!
//! Tests Proof of Life behavioral analysis against spoofing attempts:
//! - Replay previous day's sensor data (date mismatch catches it)
//! - Generate statistically valid but synthetic unlock patterns (regularity detected)
//! - Use real accelerometer recording but mismatched GPS/BT (partial pass only)
//! - Perfect synthetic data with mismatched ZK proof timestamps
//! - Behavioral recovery attack (impostor tries to recover victim's wallet)

use chrono::{Duration, TimeZone, Utc};
use gratia_core::config::ProofOfLifeConfig;
use gratia_core::{DailyProofOfLifeData, GeoLocation, OptionalSensorData};
use gratia_pol::behavioral::{
    BehavioralProfile, DailyBehavioralData,
};
use gratia_pol::behavioral_anomaly::{
    AnomalyFlag, BehavioralFingerprint, DailyBehavioralSummary,
};
use gratia_pol::validator::PolValidator;

// ============================================================================
// Helpers
// ============================================================================

fn morning_person_day() -> DailyBehavioralData {
    DailyBehavioralData {
        unlock_hours: vec![6, 6, 7, 7, 8, 8, 8, 9, 10, 11, 12, 14, 17, 21],
        interaction_sessions: vec![
            (6, 180), (7, 300), (8, 600), (9, 120), (10, 240),
            (12, 180), (14, 60), (17, 120), (21, 300),
        ],
        locations_visited: vec![
            GeoLocation { lat: 40.712, lon: -74.006 }, // Home (NYC)
            GeoLocation { lat: 40.758, lon: -73.986 }, // Work (NYC)
        ],
        charge_hours: vec![22, 7],
        unlock_count: 14,
        interaction_count: 9,
    }
}

fn night_owl_day() -> DailyBehavioralData {
    DailyBehavioralData {
        unlock_hours: vec![10, 11, 13, 15, 17, 18, 19, 19, 20, 21, 22, 22, 23, 23, 0, 1],
        interaction_sessions: vec![
            (10, 60), (13, 120), (17, 180), (19, 600), (20, 300),
            (21, 480), (22, 360), (23, 240), (0, 120),
        ],
        locations_visited: vec![
            GeoLocation { lat: 34.052, lon: -118.244 }, // Home (LA)
            GeoLocation { lat: 34.019, lon: -118.411 }, // Work (LA)
        ],
        charge_hours: vec![3, 15],
        unlock_count: 16,
        interaction_count: 9,
    }
}

#[allow(dead_code)]
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
        bt_environment_change_count: 3,
        charge_cycle_event: true,
        optional_sensors: OptionalSensorData::default(),
    }
}

fn make_realistic_day(day_offset: u32) -> DailyBehavioralSummary {
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

// ============================================================================
// Tests
// ============================================================================

/// ATTACK: Replay previous day's sensor data on a new day.
/// DEFENSE: Cross-day behavioral analysis detects identical patterns as replay.
#[test]
fn test_replay_previous_day_data_detected() {
    let mut fingerprint = BehavioralFingerprint::new();

    // Record one genuine day.
    let original_day = make_realistic_day(0);

    // Replay that exact day's data 9 more times.
    for day in 0..10 {
        let mut replayed = original_day.clone();
        replayed.date = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
            + Duration::days(day);
        fingerprint.add_day(replayed);
    }

    let anomalies = fingerprint.detect_anomalies();

    assert!(
        anomalies.contains(&AnomalyFlag::ReplayDetected),
        "Replayed data should trigger ReplayDetected, got: {:?}",
        anomalies
    );
}

/// ATTACK: Generate statistically valid but synthetic unlock patterns.
/// DEFENSE: Behavioral analysis detects suspiciously regular unlock intervals.
#[test]
fn test_synthetic_regular_unlock_patterns_detected() {
    let validator = PolValidator::new(ProofOfLifeConfig::default());
    let now = Utc::now();

    // Craft data with exactly 10 unlocks spaced perfectly over exactly 6 hours.
    // WHY: A human's phone usage is bursty — clusters of quick checks then
    // long gaps. Perfectly spaced unlocks are a strong automation signal.
    let data = DailyProofOfLifeData {
        unlock_count: 10,
        first_unlock: Some(now - Duration::hours(6)),
        last_unlock: Some(now),
        interaction_sessions: 3, // Exactly minimum
        orientation_changed: true,
        human_motion_detected: true,
        gps_fix_obtained: true,
        approximate_location: None,
        distinct_wifi_networks: 1,
        distinct_bt_environments: 2,
        bt_environment_change_count: 1,
        charge_cycle_event: true,
        optional_sensors: OptionalSensorData::default(),
    };

    let result = validator.validate_daily_data(&data);

    // The data technically passes (meets minimums)...
    assert!(result.passed);

    // ...but the suspicious pattern detector should flag it.
    assert!(
        result.suspicious_patterns.regular_interval_unlocks
            || result.suspicious_patterns.automated_cluster_pattern
            || result.suspicious_patterns.identical_session_timing,
        "Synthetic regular pattern should be flagged, got: {:?}",
        result.suspicious_patterns
    );
}

/// ATTACK: Use real accelerometer recording from a human but mismatched GPS/BT.
/// DEFENSE: PoL validates all 8 parameters independently. Missing GPS or BT
/// causes a partial failure even if motion data is genuine.
#[test]
fn test_real_motion_but_missing_gps_bt_fails() {
    let validator = PolValidator::new(ProofOfLifeConfig::default());
    let now = Utc::now();

    // Genuine motion data but no GPS fix and no BT/Wi-Fi.
    let data = DailyProofOfLifeData {
        unlock_count: 25,
        first_unlock: Some(now - Duration::hours(10)),
        last_unlock: Some(now),
        interaction_sessions: 8,
        orientation_changed: true,
        human_motion_detected: true,     // Real accelerometer data
        gps_fix_obtained: false,         // No GPS fix
        approximate_location: None,
        distinct_wifi_networks: 0,       // No Wi-Fi
        distinct_bt_environments: 0,     // No Bluetooth
        bt_environment_change_count: 0,
        charge_cycle_event: true,
        optional_sensors: OptionalSensorData::default(),
    };

    let result = validator.validate_daily_data(&data);

    assert!(
        !result.passed,
        "Missing GPS and network should cause PoL failure"
    );
    assert!(!result.parameter_results.gps_fix);
    assert!(!result.parameter_results.network_connectivity);
}

/// ATTACK: Perfect synthetic data across all 8 PoL parameters but cross-day
/// analysis reveals suspiciously low variation (identical summaries).
/// DEFENSE: Behavioral fingerprint catches the lack of natural variation.
#[test]
fn test_perfect_synthetic_data_low_variation_caught() {
    let mut fingerprint = BehavioralFingerprint::new();

    // Generate 30 days of "perfect" synthetic data that passes all checks
    // but has suspiciously low day-to-day variation.
    for day in 0..30 {
        fingerprint.add_day(DailyBehavioralSummary {
            date: Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
                + Duration::days(day),
            avg_unlock_hour: 9.0,         // Same every day
            unlock_count: 15,             // Same every day
            unlock_hour_spread: 4.0,      // Same every day
            total_screen_on_minutes: 120, // Same every day
            movement_distance_meters: 5000.0, // Same every day
            bluetooth_peer_diversity: 8,  // Same every day
            charge_start_hour: 22.0,      // Same every day
            interaction_richness: 0.65,   // Same every day
        });
    }

    let anomalies = fingerprint.detect_anomalies();

    // WHY: Identical data across 30 days should trigger replay detection.
    // Real humans have natural day-to-day variation.
    assert!(
        anomalies.contains(&AnomalyFlag::ReplayDetected),
        "Identical daily data should trigger ReplayDetected, got: {:?}",
        anomalies
    );

    // The variation score should be penalized (0 for the variation component).
    assert!(
        fingerprint.score() < 75,
        "Identical daily patterns should produce lower score, got: {}",
        fingerprint.score()
    );
}

/// ATTACK: Behavioral recovery attack — impostor tries to recover victim's wallet.
/// DEFENSE: Behavioral profile comparison detects different usage patterns
/// over the 7-14 day recovery window.
#[test]
fn test_behavioral_recovery_attack_impostor_rejected() {
    let mut victim_profile = BehavioralProfile::new();
    let mut impostor_profile = BehavioralProfile::new();

    // Victim is a morning person in NYC, builds 30-day profile.
    for _ in 0..30 {
        victim_profile.update_profile(&morning_person_day());
    }

    // Impostor is a night owl in LA, tries recovery over 10 days.
    for _ in 0..10 {
        impostor_profile.update_profile(&night_owl_day());
    }

    assert!(impostor_profile.is_mature(), "Impostor profile should be mature (7+ days)");

    let similarity = victim_profile.compare_profiles(&impostor_profile);
    let threshold = BehavioralProfile::default_match_threshold();

    assert!(
        similarity < threshold,
        "Impostor should be BELOW recovery threshold. Similarity: {:.3}, threshold: {:.3}",
        similarity,
        threshold
    );
}

/// CONTROL: Same person recovers their own wallet — behavioral match succeeds.
#[test]
fn test_behavioral_recovery_legitimate_owner_accepted() {
    let mut original_profile = BehavioralProfile::new();
    let mut recovery_profile = BehavioralProfile::new();

    // Build original profile over 30 days.
    for _ in 0..30 {
        original_profile.update_profile(&morning_person_day());
    }

    // Build recovery profile over 10 days with slight natural variation.
    for i in 0..10 {
        let mut day = morning_person_day();
        // Slight variation: occasional extra activity.
        if i % 3 == 0 {
            day.unlock_hours.push(15);
            day.unlock_count += 1;
        }
        if i % 4 == 0 {
            day.interaction_sessions.push((16, 90));
            day.interaction_count += 1;
        }
        recovery_profile.update_profile(&day);
    }

    let similarity = original_profile.compare_profiles(&recovery_profile);
    let threshold = BehavioralProfile::default_match_threshold();

    assert!(
        similarity >= threshold,
        "Legitimate owner should pass recovery. Similarity: {:.3}, threshold: {:.3}",
        similarity,
        threshold
    );
}

/// ATTACK: Impostor mimics victim's general schedule but from a different city
/// with slightly different timing (shifted by a few hours).
/// DEFENSE: Combined location mismatch + timing drift pushes score below threshold.
#[test]
fn test_behavioral_recovery_wrong_location_rejected() {
    let mut victim_profile = BehavioralProfile::new();
    let mut impostor_profile = BehavioralProfile::new();

    // Victim: morning person in NYC.
    for _ in 0..30 {
        victim_profile.update_profile(&morning_person_day());
    }

    // Impostor: similar-ish timing but shifted by 2-3 hours, different city,
    // and different activity levels. This is a more realistic impostor
    // who looked up the victim's general habits but can't perfectly replicate them.
    for _ in 0..10 {
        let day = DailyBehavioralData {
            unlock_hours: vec![8, 9, 9, 10, 10, 11, 11, 12, 13, 15, 18, 20, 23],
            interaction_sessions: vec![
                (9, 200), (10, 250), (11, 400), (13, 100), (15, 150),
                (18, 200), (20, 120), (23, 250),
            ],
            locations_visited: vec![
                GeoLocation { lat: 41.878, lon: -87.629 }, // Chicago
                GeoLocation { lat: 41.881, lon: -87.623 }, // Chicago work
            ],
            charge_hours: vec![0, 9],
            unlock_count: 13,
            interaction_count: 8,
        };
        impostor_profile.update_profile(&day);
    }

    let similarity = victim_profile.compare_profiles(&impostor_profile);
    let threshold = BehavioralProfile::default_match_threshold();

    // WHY: Location mismatch (25% weight, ~0.0 for NYC vs Chicago) combined
    // with shifted timing and different activity levels should bring
    // the composite score below the 0.65 threshold.
    assert!(
        similarity < threshold,
        "Wrong-location impostor should be below threshold. \
         Similarity: {:.3}, threshold: {:.3}",
        similarity,
        threshold
    );
}

/// ATTACK: Attacker replays PoL data but with different behavioral summary timestamps.
/// DEFENSE: Behavioral discontinuity detection catches the pattern shift.
#[test]
fn test_behavioral_discontinuity_from_different_operator() {
    let mut fingerprint = BehavioralFingerprint::new();

    // First 15 days: one person (early riser, low activity).
    // WHY: Using make_realistic_day as base and overriding key metrics ensures
    // natural variation within each half (avoiding replay detection), while
    // the two halves differ dramatically from each other.
    for day in 0..15 {
        let mut summary = make_realistic_day(day);
        summary.avg_unlock_hour = 6.5 + (day as f32 % 3.0) * 0.3;
        summary.unlock_count = 15 + (day % 4) as u32;
        summary.movement_distance_meters = 2000.0 + (day as f64) * 50.0;
        summary.charge_start_hour = 21.0 + (day as f32 % 3.0) * 0.5;
        summary.interaction_richness = 0.85 - (day as f32 % 5.0) * 0.02;
        fingerprint.add_day(summary);
    }

    // Last 15 days: different person takes over (night owl, high activity).
    for day in 15..30 {
        let mut summary = make_realistic_day(day);
        summary.avg_unlock_hour = 18.0 + (day as f32 % 3.0) * 0.3;
        summary.unlock_count = 70 + (day % 5) as u32;
        summary.movement_distance_meters = 15000.0 + (day as f64) * 100.0;
        summary.charge_start_hour = 5.0 + (day as f32 % 3.0) * 0.5;
        summary.interaction_richness = 0.3 + (day as f32 % 4.0) * 0.02;
        fingerprint.add_day(summary);
    }

    let anomalies = fingerprint.detect_anomalies();

    assert!(
        anomalies.contains(&AnomalyFlag::BehavioralDiscontinuity),
        "Different operator should trigger BehavioralDiscontinuity, got: {:?}",
        anomalies
    );
}
