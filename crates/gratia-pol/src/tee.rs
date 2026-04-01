//! TEE (Trusted Execution Environment) attestation verification.
//!
//! Verifies device integrity attestations from Android Play Integrity,
//! Android hardware KeyStore/StrongBox, and iOS App Attest. TEE status
//! directly affects the node's Presence Score and the scrutiny level
//! applied by the protocol.
//!
//! Key design principle: a device that HAS TEE but FAILS its checks is
//! penalized more harshly than a device that simply lacks TEE. A failed
//! TEE strongly suggests rooting, emulation, or app tampering — deliberate
//! evasion rather than hardware limitations.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Attestation staleness threshold
// ============================================================================

/// Maximum age of a TEE attestation before it is considered stale.
/// WHY: 24 hours aligns with the daily PoL cycle. An attestation older than
/// one full cycle could have been captured before a device was rooted or
/// compromised, so we downgrade trust rather than reject outright.
const ATTESTATION_MAX_AGE_HOURS: i64 = 24;

// ============================================================================
// Presence Score adjustments
// ============================================================================

/// Presence Score bonus for full TEE (all checks pass + hardware sensor attestation).
/// WHY: +8 matches the existing Presence Score weight for secure_enclave in
/// SensorFlags::calculate_score, keeping the two systems consistent.
const FULL_TEE_SCORE: i8 = 8;

/// Presence Score bonus for basic TEE (passes but lacks hardware sensor attestation).
/// WHY: +5 rewards having TEE without giving full credit — the device is
/// trustworthy but cannot hardware-attest individual sensor readings.
const BASIC_TEE_SCORE: i8 = 5;

/// Presence Score penalty for absent TEE (no attestation provided).
/// WHY: -8 is moderate. Many legitimate devices (budget phones, custom ROMs)
/// lack TEE. The penalty increases scrutiny without being punitive enough to
/// exclude them from meaningful participation.
const ABSENT_TEE_SCORE: i8 = -8;

/// Presence Score penalty for failed TEE (attestation present but checks fail).
/// WHY: -15 is intentionally harsher than Absent (-8). A device that HAS TEE
/// but fails integrity checks is far more suspicious than one that simply
/// doesn't have TEE. Failed TEE likely means rooting, emulation, or binary
/// tampering — all deliberate evasion techniques.
const FAILED_TEE_SCORE: i8 = -15;

// ============================================================================
// TeeProvider
// ============================================================================

/// The TEE attestation provider used by the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeeProvider {
    /// Android Play Integrity API (replaces SafetyNet).
    AndroidPlayIntegrity,
    /// Android hardware-backed KeyStore/StrongBox attestation.
    AndroidKeyAttestation,
    /// iOS DeviceCheck / App Attest.
    IosAppAttest,
    /// No TEE attestation available.
    None,
}

// ============================================================================
// TeeAttestation
// ============================================================================

/// Raw TEE attestation data submitted by a node.
///
/// This struct arrives from the mobile app layer via UniFFI. The attestation
/// token is opaque bytes whose format depends on the provider — the protocol
/// verifies it against the provider's public key infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeeAttestation {
    /// The attestation provider.
    pub provider: TeeProvider,
    /// Whether the device passed integrity verification.
    pub device_integrity_passed: bool,
    /// Whether the app binary is genuine (not modified).
    pub app_integrity_passed: bool,
    /// Whether the device has a hardware-backed secure enclave.
    pub has_secure_enclave: bool,
    /// Whether hardware-backed sensor attestation is available (Android 13+).
    pub has_hardware_sensor_attestation: bool,
    /// Raw attestation token (opaque bytes — verified by the protocol).
    pub attestation_token: Vec<u8>,
    /// Timestamp of the attestation.
    pub attested_at: DateTime<Utc>,
    /// Whether the device appears to be rooted/jailbroken.
    pub is_rooted: bool,
    /// Whether the device appears to be an emulator.
    pub is_emulator: bool,
}

// ============================================================================
// TeeTrustLevel
// ============================================================================

/// Overall trust level derived from TEE attestation verification.
///
/// Ordered from lowest to highest trust so that comparisons (`>=`, `<`) work
/// naturally on the derived `Ord` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TeeTrustLevel {
    /// No TEE attestation. Maximum additional scrutiny.
    Absent,
    /// TEE present but integrity checks failed (rooted, modified app).
    Failed,
    /// TEE present, basic checks pass but no hardware attestation.
    Basic,
    /// TEE present, all checks pass including hardware sensor attestation.
    Full,
}

// ============================================================================
// TeeFlag
// ============================================================================

/// Individual flags raised during TEE attestation verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeeFlag {
    /// Device appears to be rooted or jailbroken.
    RootDetected,
    /// Device appears to be running in an emulator.
    EmulatorDetected,
    /// App binary has been modified (repackaged, patched, or injected).
    AppTampered,
    /// No hardware-backed secure enclave available.
    NoSecureEnclave,
    /// Attestation is older than 24 hours.
    StaleAttestation,
    /// Provider is TeeProvider::None — no attestation submitted.
    MissingProvider,
}

// ============================================================================
// ScrutinyModifier
// ============================================================================

/// TEE-derived modifier to the node's scrutiny level.
///
/// This is combined with the trust-tier scrutiny from `trust.rs` to produce
/// the final scrutiny applied to the node. A node can have `Standard` TEE
/// scrutiny but still face `Maximum` trust-tier scrutiny if it is brand-new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrutinyModifier {
    /// Node passes all TEE checks. Standard scrutiny.
    Standard,
    /// Node has basic TEE. Slightly elevated scrutiny.
    Elevated,
    /// Node has no TEE or failed TEE. Maximum additional scrutiny.
    Maximum,
}

// ============================================================================
// TeeVerificationResult
// ============================================================================

/// The outcome of verifying a node's TEE attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeVerificationResult {
    /// Overall TEE trust level.
    pub trust_level: TeeTrustLevel,
    /// Flags detected during verification.
    pub flags: Vec<TeeFlag>,
    /// Presence Score adjustment from TEE.
    pub presence_score_adjustment: i8,
    /// Scrutiny level modifier.
    pub scrutiny_modifier: ScrutinyModifier,
}

// ============================================================================
// Verification logic
// ============================================================================

/// Verify a TEE attestation and produce a trust assessment.
///
/// Evaluation order matters — early checks for the most severe violations
/// (rooting, emulation, app tampering) short-circuit before softer checks
/// (staleness, missing enclave). This ensures the worst flags are always
/// surfaced even if multiple issues exist.
pub fn verify_tee_attestation(attestation: &TeeAttestation) -> TeeVerificationResult {
    let mut flags: Vec<TeeFlag> = Vec::new();

    // --- No TEE provider at all ---
    if attestation.provider == TeeProvider::None {
        flags.push(TeeFlag::MissingProvider);
        return TeeVerificationResult {
            trust_level: TeeTrustLevel::Absent,
            flags,
            presence_score_adjustment: ABSENT_TEE_SCORE,
            scrutiny_modifier: ScrutinyModifier::Maximum,
        };
    }

    // --- Rooted / jailbroken device ---
    if attestation.is_rooted {
        flags.push(TeeFlag::RootDetected);
    }

    // --- Emulator detected ---
    if attestation.is_emulator {
        flags.push(TeeFlag::EmulatorDetected);
    }

    // WHY: Root and emulator are checked together because a device can be
    // both (rooted emulator). We collect all flags before returning so the
    // caller gets a complete picture.
    if attestation.is_rooted || attestation.is_emulator {
        return TeeVerificationResult {
            trust_level: TeeTrustLevel::Failed,
            flags,
            presence_score_adjustment: FAILED_TEE_SCORE,
            scrutiny_modifier: ScrutinyModifier::Maximum,
        };
    }

    // --- App binary tampered ---
    if !attestation.app_integrity_passed {
        flags.push(TeeFlag::AppTampered);
        return TeeVerificationResult {
            trust_level: TeeTrustLevel::Failed,
            flags,
            presence_score_adjustment: FAILED_TEE_SCORE,
            scrutiny_modifier: ScrutinyModifier::Maximum,
        };
    }

    // --- Stale attestation (older than 24 hours) ---
    let age = Utc::now() - attestation.attested_at;
    let is_stale = age > Duration::hours(ATTESTATION_MAX_AGE_HOURS);
    if is_stale {
        flags.push(TeeFlag::StaleAttestation);
    }

    // --- No secure enclave ---
    // WHY: Collect this flag BEFORE checking staleness so both flags are reported
    // when an attestation is both stale AND lacks secure enclave.
    if !attestation.has_secure_enclave {
        flags.push(TeeFlag::NoSecureEnclave);
    }

    // --- Stale attestation downgrades Full -> Basic ---
    if is_stale {
        return TeeVerificationResult {
            trust_level: TeeTrustLevel::Basic,
            flags,
            presence_score_adjustment: BASIC_TEE_SCORE,
            scrutiny_modifier: ScrutinyModifier::Standard,
        };
    }

    // --- No secure enclave (not stale but missing enclave) ---
    if !attestation.has_secure_enclave {
        // WHY: A device without a secure enclave but with valid TEE otherwise
        // gets Basic trust and Elevated scrutiny. The device is probably genuine
        // but cannot provide hardware-backed key isolation, so we trust it less.
        return TeeVerificationResult {
            trust_level: TeeTrustLevel::Basic,
            flags,
            presence_score_adjustment: 0,
            scrutiny_modifier: ScrutinyModifier::Elevated,
        };
    }

    // --- Basic TEE: all checks pass but no hardware sensor attestation ---
    if !attestation.has_hardware_sensor_attestation {
        return TeeVerificationResult {
            trust_level: TeeTrustLevel::Basic,
            flags,
            presence_score_adjustment: BASIC_TEE_SCORE,
            scrutiny_modifier: ScrutinyModifier::Standard,
        };
    }

    // --- Full TEE: everything passes ---
    TeeVerificationResult {
        trust_level: TeeTrustLevel::Full,
        flags,
        presence_score_adjustment: FULL_TEE_SCORE,
        scrutiny_modifier: ScrutinyModifier::Standard,
    }
}

/// Map a TEE trust level to its Presence Score adjustment.
///
/// WHY: The -15 for Failed is intentional — a device that HAS TEE but fails
/// it is more suspicious than one that simply doesn't have TEE (custom ROM,
/// budget phone). A failed TEE likely means rooting or tampering.
pub fn presence_score_for_tee(trust_level: TeeTrustLevel) -> i8 {
    match trust_level {
        TeeTrustLevel::Full => FULL_TEE_SCORE,
        TeeTrustLevel::Basic => BASIC_TEE_SCORE,
        TeeTrustLevel::Failed => FAILED_TEE_SCORE,
        TeeTrustLevel::Absent => ABSENT_TEE_SCORE,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    /// Helper: build a fully-passing TEE attestation (Android, all checks green).
    fn full_attestation() -> TeeAttestation {
        TeeAttestation {
            provider: TeeProvider::AndroidPlayIntegrity,
            device_integrity_passed: true,
            app_integrity_passed: true,
            has_secure_enclave: true,
            has_hardware_sensor_attestation: true,
            attestation_token: vec![0xDE, 0xAD, 0xBE, 0xEF],
            attested_at: Utc::now(),
            is_rooted: false,
            is_emulator: false,
        }
    }

    #[test]
    fn test_full_tee_gives_highest_score() {
        let result = verify_tee_attestation(&full_attestation());
        assert_eq!(result.trust_level, TeeTrustLevel::Full);
        assert_eq!(result.presence_score_adjustment, FULL_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Standard);
        assert!(result.flags.is_empty());
    }

    #[test]
    fn test_absent_tee_reduces_score() {
        let attestation = TeeAttestation {
            provider: TeeProvider::None,
            device_integrity_passed: false,
            app_integrity_passed: false,
            has_secure_enclave: false,
            has_hardware_sensor_attestation: false,
            attestation_token: vec![],
            attested_at: Utc::now(),
            is_rooted: false,
            is_emulator: false,
        };
        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Absent);
        assert_eq!(result.presence_score_adjustment, ABSENT_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Maximum);
        assert!(result.flags.contains(&TeeFlag::MissingProvider));
    }

    #[test]
    fn test_rooted_device_flags_and_penalizes() {
        let mut attestation = full_attestation();
        attestation.is_rooted = true;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Failed);
        assert_eq!(result.presence_score_adjustment, FAILED_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Maximum);
        assert!(result.flags.contains(&TeeFlag::RootDetected));
    }

    #[test]
    fn test_emulator_detected() {
        let mut attestation = full_attestation();
        attestation.is_emulator = true;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Failed);
        assert_eq!(result.presence_score_adjustment, FAILED_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Maximum);
        assert!(result.flags.contains(&TeeFlag::EmulatorDetected));
    }

    #[test]
    fn test_stale_attestation_downgrades_to_basic() {
        let mut attestation = full_attestation();
        // WHY: 25 hours ago puts it just past the 24-hour threshold.
        attestation.attested_at = Utc::now() - Duration::hours(25);

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Basic);
        assert!(result.flags.contains(&TeeFlag::StaleAttestation));
        assert_eq!(result.presence_score_adjustment, BASIC_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Standard);
    }

    #[test]
    fn test_no_secure_enclave_is_elevated() {
        let mut attestation = full_attestation();
        attestation.has_secure_enclave = false;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Basic);
        assert!(result.flags.contains(&TeeFlag::NoSecureEnclave));
        assert_eq!(result.presence_score_adjustment, 0);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Elevated);
    }

    #[test]
    fn test_app_tampered_is_maximum_scrutiny() {
        let mut attestation = full_attestation();
        attestation.app_integrity_passed = false;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Failed);
        assert_eq!(result.presence_score_adjustment, FAILED_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Maximum);
        assert!(result.flags.contains(&TeeFlag::AppTampered));
    }

    #[test]
    fn test_failed_worse_than_absent() {
        // WHY: A device that HAS TEE but fails it (-15) should be penalized
        // more than a device that simply doesn't have TEE (-8). This confirms
        // the asymmetric penalty design.
        let failed_score = presence_score_for_tee(TeeTrustLevel::Failed);
        let absent_score = presence_score_for_tee(TeeTrustLevel::Absent);
        assert!(
            failed_score < absent_score,
            "Failed TEE ({}) should be worse (more negative) than Absent TEE ({})",
            failed_score,
            absent_score,
        );
    }

    #[test]
    fn test_basic_tee_without_hardware_sensor_attestation() {
        let mut attestation = full_attestation();
        attestation.has_hardware_sensor_attestation = false;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Basic);
        assert_eq!(result.presence_score_adjustment, BASIC_TEE_SCORE);
        assert_eq!(result.scrutiny_modifier, ScrutinyModifier::Standard);
        assert!(result.flags.is_empty());
    }

    #[test]
    fn test_rooted_emulator_collects_both_flags() {
        let mut attestation = full_attestation();
        attestation.is_rooted = true;
        attestation.is_emulator = true;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Failed);
        assert!(result.flags.contains(&TeeFlag::RootDetected));
        assert!(result.flags.contains(&TeeFlag::EmulatorDetected));
        assert_eq!(result.flags.len(), 2);
    }

    #[test]
    fn test_trust_level_ordering() {
        // Verify that the derived Ord gives the expected ordering.
        assert!(TeeTrustLevel::Absent < TeeTrustLevel::Failed);
        assert!(TeeTrustLevel::Failed < TeeTrustLevel::Basic);
        assert!(TeeTrustLevel::Basic < TeeTrustLevel::Full);
    }

    #[test]
    fn test_presence_score_for_tee_all_levels() {
        assert_eq!(presence_score_for_tee(TeeTrustLevel::Full), 8);
        assert_eq!(presence_score_for_tee(TeeTrustLevel::Basic), 5);
        assert_eq!(presence_score_for_tee(TeeTrustLevel::Absent), -8);
        assert_eq!(presence_score_for_tee(TeeTrustLevel::Failed), -15);
    }

    #[test]
    fn test_ios_provider_full_trust() {
        let mut attestation = full_attestation();
        attestation.provider = TeeProvider::IosAppAttest;

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Full);
        assert_eq!(result.presence_score_adjustment, FULL_TEE_SCORE);
    }

    #[test]
    fn test_fresh_attestation_not_stale() {
        // An attestation from 23 hours ago should NOT be stale.
        let mut attestation = full_attestation();
        attestation.attested_at = Utc::now() - Duration::hours(23);

        let result = verify_tee_attestation(&attestation);
        assert_eq!(result.trust_level, TeeTrustLevel::Full);
        assert!(!result.flags.contains(&TeeFlag::StaleAttestation));
    }
}
