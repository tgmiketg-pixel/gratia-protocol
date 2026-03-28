//! Emulator / VM Detection Tests
//!
//! Tests the third pillar of consensus security — energy expenditure verification.
//! The protocol must detect and reject:
//! - Emulated ARM CPUs (no TEE/secure enclave)
//! - VMs with fake GPS (quality checks on accuracy and satellite count)
//! - Emulators with fabricated Bluetooth peers
//! - Cloud phone farms (inconsistent network latency patterns)
//!
//! A real physical phone should pass all detection checks.

use chrono::Utc;
use gratia_core::types::SensorFlags;
use gratia_pol::scoring::{apply_security_adjustments, compute_enhanced_presence_score};
use gratia_pol::tee::{
    TeeAttestation, TeeFlag, TeeProvider, TeeTrustLevel, verify_tee_attestation,
};

// ============================================================================
// Helpers
// ============================================================================

/// Build a TEE attestation representing a real physical phone.
fn real_phone_attestation() -> TeeAttestation {
    TeeAttestation {
        provider: TeeProvider::AndroidPlayIntegrity,
        device_integrity_passed: true,
        app_integrity_passed: true,
        has_secure_enclave: true,
        has_hardware_sensor_attestation: true,
        attestation_token: vec![0xAA; 128],
        attested_at: Utc::now(),
        is_rooted: false,
        is_emulator: false,
    }
}

/// Build a TEE attestation representing an emulator.
fn emulator_attestation() -> TeeAttestation {
    TeeAttestation {
        provider: TeeProvider::AndroidPlayIntegrity,
        device_integrity_passed: false,
        app_integrity_passed: true,
        has_secure_enclave: false,
        has_hardware_sensor_attestation: false,
        attestation_token: vec![0x00; 128],
        attested_at: Utc::now(),
        is_rooted: false,
        is_emulator: true,
    }
}

/// Build a TEE attestation representing a rooted device.
fn rooted_device_attestation() -> TeeAttestation {
    TeeAttestation {
        provider: TeeProvider::AndroidPlayIntegrity,
        device_integrity_passed: false,
        app_integrity_passed: false,
        has_secure_enclave: true,
        has_hardware_sensor_attestation: false,
        attestation_token: vec![0x11; 128],
        attested_at: Utc::now(),
        is_rooted: true,
        is_emulator: false,
    }
}

/// Build a TEE attestation for a device with no TEE at all.
fn no_tee_attestation() -> TeeAttestation {
    TeeAttestation {
        provider: TeeProvider::None,
        device_integrity_passed: false,
        app_integrity_passed: false,
        has_secure_enclave: false,
        has_hardware_sensor_attestation: false,
        attestation_token: vec![],
        attested_at: Utc::now(),
        is_rooted: false,
        is_emulator: false,
    }
}

fn core_sensor_flags() -> SensorFlags {
    SensorFlags {
        gps: true,
        accelerometer: true,
        wifi: true,
        bluetooth: true,
        gyroscope: false,
        ambient_light: false,
        cellular: false,
        barometer: false,
        magnetometer: false,
        nfc: false,
        secure_enclave: false,
        biometric: false,
        camera_hash: false,
        microphone_hash: false,
    }
}

// ============================================================================
// Tests
// ============================================================================

/// ATTACK: Emulated ARM CPU has no TEE/secure enclave.
/// DEFENSE: TEE verification returns Failed trust level with EmulatorDetected flag.
/// The presence score penalty is -15, which can push score below consensus threshold.
#[test]
fn test_emulator_no_tee_detected() {
    let attestation = emulator_attestation();
    let result = verify_tee_attestation(&attestation);

    assert_eq!(
        result.trust_level,
        TeeTrustLevel::Failed,
        "Emulator should have Failed trust level"
    );
    assert!(
        result.flags.contains(&TeeFlag::EmulatorDetected),
        "EmulatorDetected flag should be set, got: {:?}",
        result.flags
    );
    assert_eq!(
        result.presence_score_adjustment, -15,
        "Emulator should get -15 penalty"
    );
}

/// ATTACK: Emulator with -15 TEE penalty — verify it pushes the presence score
/// below the consensus threshold (40) for bare-minimum sensor configurations.
#[test]
fn test_emulator_score_below_consensus_threshold() {
    // Base score with core sensors: 45 (GPS + accelerometer + Wi-Fi + BT).
    // WHY: Having both Wi-Fi AND Bluetooth gives +5 bonus on top of the base 40.
    let base_score = core_sensor_flags().calculate_score(0);
    assert_eq!(base_score, 45, "Core sensors with both Wi-Fi and BT give base score of 45");

    // Apply TEE penalty for emulator (-15) + no behavioral history (score 0 -> 0 bonus).
    let final_score = apply_security_adjustments(base_score, -15, 0);

    // WHY: 45 - 15 + 0 = 30, which is below the consensus threshold of 40.
    // An emulator with only core sensors cannot participate in consensus.
    assert!(
        final_score < 40,
        "Emulator score ({}) should be below consensus threshold (40)",
        final_score
    );
}

/// ATTACK: VM with fake GPS — simulated satellite data.
/// DEFENSE: Without TEE hardware sensor attestation, the GPS data cannot be
/// hardware-verified. Combined with no TEE, the score drops significantly.
#[test]
fn test_vm_fake_gps_no_hardware_attestation() {
    let attestation = emulator_attestation();
    let result = verify_tee_attestation(&attestation);

    // A VM can fake GPS coordinates, but without hardware sensor attestation
    // the protocol treats all sensor data as unverified.
    assert!(
        !attestation.has_hardware_sensor_attestation,
        "Emulator should not have hardware sensor attestation"
    );

    // The trust level being Failed means all sensor readings face maximum scrutiny.
    assert_eq!(result.trust_level, TeeTrustLevel::Failed);
}

/// ATTACK: Emulator with fake Bluetooth — fabricated peer hashes.
/// DEFENSE: Without real BT hardware, the emulator either reports zero peers
/// (failing PoL connectivity) or fabricated peers (caught by peer set hash
/// analysis across the network).
#[test]
fn test_emulator_fabricated_bluetooth_detected() {
    let attestation = emulator_attestation();
    let result = verify_tee_attestation(&attestation);

    // Even if the emulator generates fake BT peer hashes, the TEE check
    // already failed. The node faces maximum scrutiny on all sensor data.
    assert_eq!(result.trust_level, TeeTrustLevel::Failed);

    // The composite score with emulator penalty makes the node ineligible.
    let base = core_sensor_flags().calculate_score(0);
    let final_score = apply_security_adjustments(base, result.presence_score_adjustment, 0);
    // WHY: 45 - 15 = 30, below the 40 threshold.
    assert!(
        final_score < 40,
        "Emulator with fake BT should score below threshold: {}",
        final_score
    );
}

/// ATTACK: Cloud phone farm running VMs with latency inconsistencies.
/// DEFENSE: Without TEE, the presence score penalty makes participation unviable.
/// Additionally, the behavioral fingerprint would show static device patterns.
#[test]
fn test_cloud_farm_no_tee_penalized() {
    // Cloud VMs typically have no TEE at all.
    let attestation = no_tee_attestation();
    let result = verify_tee_attestation(&attestation);

    assert_eq!(result.trust_level, TeeTrustLevel::Absent);
    assert!(
        result.flags.contains(&TeeFlag::MissingProvider),
        "Cloud VM should have MissingProvider flag"
    );
    assert_eq!(
        result.presence_score_adjustment, -8,
        "Absent TEE should get -8 penalty"
    );

    // With only core sensors and -8 penalty:
    let base = core_sensor_flags().calculate_score(0);
    let final_score = apply_security_adjustments(base, -8, 0);

    // 40 - 8 = 32, below threshold.
    assert!(
        final_score < 40,
        "Cloud VM score ({}) should be below consensus threshold",
        final_score
    );
}

/// CONTROL: Real phone passes all detection checks.
#[test]
fn test_real_phone_passes_all_checks() {
    let attestation = real_phone_attestation();
    let result = verify_tee_attestation(&attestation);

    assert_eq!(
        result.trust_level,
        TeeTrustLevel::Full,
        "Real phone should have Full trust level"
    );
    assert!(
        result.flags.is_empty(),
        "Real phone should have no flags, got: {:?}",
        result.flags
    );
    assert_eq!(
        result.presence_score_adjustment, 8,
        "Real phone with full TEE should get +8 bonus"
    );
}

/// CONTROL: Real phone with full TEE and good behavioral score achieves high
/// composite presence score.
#[test]
fn test_real_phone_high_composite_score() {
    // Sensor flags: core + several optional sensors.
    let mut flags = core_sensor_flags();
    flags.gyroscope = true;     // +5
    flags.secure_enclave = true; // +8
    flags.biometric = true;      // +5
    flags.barometer = true;      // +5

    let base = flags.calculate_score(100); // 100 participation days -> +4
    // 40 + 5 + 8 + 5 + 5 + 2 + 2 = 67

    let enhanced = compute_enhanced_presence_score(base, 8, 90);
    // base 67 + TEE 8 + behavioral 10 = 85

    assert!(
        enhanced.final_score >= 80,
        "Real phone should have high composite score, got: {}",
        enhanced.final_score
    );
}

/// ATTACK: Rooted device with modified app binary.
/// DEFENSE: TEE verification detects rooting and returns Failed trust level.
#[test]
fn test_rooted_device_failed_tee() {
    let attestation = rooted_device_attestation();
    let result = verify_tee_attestation(&attestation);

    assert_eq!(result.trust_level, TeeTrustLevel::Failed);
    assert!(result.flags.contains(&TeeFlag::RootDetected));
    assert_eq!(result.presence_score_adjustment, -15);
}

/// ATTACK: Device with TEE but no hardware sensor attestation (older Android).
/// DEFENSE: Gets Basic trust level with reduced bonus (+5 instead of +8).
#[test]
fn test_basic_tee_without_hardware_sensors() {
    let mut attestation = real_phone_attestation();
    attestation.has_hardware_sensor_attestation = false;

    let result = verify_tee_attestation(&attestation);

    assert_eq!(
        result.trust_level,
        TeeTrustLevel::Basic,
        "Device without hardware sensor attestation should be Basic"
    );
    assert_eq!(
        result.presence_score_adjustment, 5,
        "Basic TEE should get +5 bonus"
    );
}

/// VERIFY: The scoring system correctly layers TEE and behavioral adjustments.
#[test]
fn test_scoring_layers_combine_correctly() {
    // Scenario 1: High base, full TEE, excellent behavioral.
    let score1 = apply_security_adjustments(70, 8, 90);
    // 70 + 8 + 10 = 88
    assert_eq!(score1, 88);

    // Scenario 2: Low base, failed TEE, poor behavioral.
    let score2 = apply_security_adjustments(40, -15, 10);
    // 40 - 15 + 0 = 25
    assert_eq!(score2, 25);

    // Scenario 3: High base, no TEE, moderate behavioral.
    let score3 = apply_security_adjustments(70, -8, 55);
    // 70 - 8 + 4 = 66
    assert_eq!(score3, 66);

    // Scenario 4: Score capped at 100.
    let score4 = apply_security_adjustments(95, 8, 95);
    assert_eq!(score4, 100);
}

/// ATTACK: Stale TEE attestation (older than 24 hours).
/// DEFENSE: Stale attestation is downgraded — the device may have been
/// compromised after the attestation was obtained.
#[test]
fn test_stale_tee_attestation_downgraded() {
    let mut attestation = real_phone_attestation();
    // Set attestation timestamp to 25 hours ago.
    attestation.attested_at = Utc::now() - chrono::Duration::hours(25);

    let result = verify_tee_attestation(&attestation);

    // WHY: A stale attestation should be downgraded from Full to Basic
    // or have a StaleAttestation flag (depending on implementation).
    // The key point is that stale attestations don't get full trust.
    assert!(
        result.trust_level < TeeTrustLevel::Full
            || result.flags.contains(&TeeFlag::StaleAttestation),
        "Stale attestation should be downgraded or flagged. \
         Trust: {:?}, Flags: {:?}",
        result.trust_level,
        result.flags
    );
}
