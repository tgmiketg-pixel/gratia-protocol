//! Composite Presence Score calculation for the Gratia protocol.
//!
//! The Presence Score (40-100) determines block production selection probability
//! via VRF weighting. It does NOT affect mining rewards — those are flat for all
//! nodes that pass the binary threshold.
//!
//! The score has three layers:
//! 1. **Base score** — derived from sensor hardware capabilities (existing system).
//! 2. **TEE attestation adjustment** — rewards/penalizes based on device integrity.
//! 3. **Behavioral consistency bonus** — rewards sustained human-like usage patterns.
//!
//! Layers 2 and 3 are applied as post-hoc adjustments to the base score because
//! they represent different security concerns than hardware capability.

use serde::{Deserialize, Serialize};

// ============================================================================
// Enhanced Presence Score (with TEE + behavioral adjustments)
// ============================================================================

/// Breakdown of an Enhanced Presence Score showing all components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedPresenceScore {
    /// Base sensor-derived score (40-100).
    pub base_score: u8,
    /// TEE attestation adjustment (-15 to +8).
    pub tee_adjustment: i8,
    /// Behavioral consistency bonus (0-10).
    pub behavioral_bonus: i16,
    /// Raw behavioral consistency score (0-100).
    pub behavioral_raw: u8,
    /// Final composite score after all adjustments.
    pub final_score: u8,
}

// ============================================================================
// Security adjustment functions
// ============================================================================

/// Apply TEE attestation and behavioral consistency adjustments to a base Presence Score.
///
/// This is called AFTER the base sensor-based score is computed.
/// TEE attestation can add +8 (full), +5 (basic), -8 (absent), or -15 (failed).
/// Behavioral consistency adds 0-10 points based on the 30-day consistency score.
///
/// WHY: TEE and behavioral scores are applied as adjustments rather than built into
/// the base score because they represent different security layers. The base score
/// is "does this device have the right hardware?" TEE is "is this device trustworthy?"
/// Behavioral is "does this device's usage pattern look real over time?"
pub fn apply_security_adjustments(
    base_score: u8,
    tee_adjustment: i8,
    behavioral_consistency_score: u8,
) -> u8 {
    // WHY: Behavioral consistency contributes up to 10 extra points.
    // Score of 80+ = 10 pts, 60-79 = 7 pts, 40-59 = 4 pts, <40 = 0 pts.
    let behavioral_bonus: i16 = match behavioral_consistency_score {
        80..=100 => 10,
        60..=79 => 7,
        40..=59 => 4,
        _ => 0,
    };

    let adjusted = base_score as i16 + tee_adjustment as i16 + behavioral_bonus;

    // WHY: Clamp to valid Presence Score range (0-100).
    // Below 40 = ineligible for consensus (handled by EligibleNode::is_eligible).
    // Above 100 = cap at maximum.
    adjusted.clamp(0, 100) as u8
}

/// Compute a complete Enhanced Presence Score including all security layers.
///
/// Returns an `EnhancedPresenceScore` that breaks down each component for
/// transparency and debugging. The `final_score` field is the value used
/// for VRF weighting in block producer selection.
pub fn compute_enhanced_presence_score(
    base_score: u8,
    tee_adjustment: i8,
    behavioral_consistency_score: u8,
) -> EnhancedPresenceScore {
    let final_score = apply_security_adjustments(base_score, tee_adjustment, behavioral_consistency_score);

    let behavioral_bonus = match behavioral_consistency_score {
        80..=100 => 10,
        60..=79 => 7,
        40..=59 => 4,
        _ => 0,
    };

    EnhancedPresenceScore {
        base_score,
        tee_adjustment,
        behavioral_bonus,
        behavioral_raw: behavioral_consistency_score,
        final_score,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_tee_boosts_score() {
        // Base 60 + TEE +8 + behavioral 85 (-> +10) = 78
        let score = apply_security_adjustments(60, 8, 85);
        assert_eq!(score, 78);
    }

    #[test]
    fn test_absent_tee_reduces_score() {
        // Base 60 + TEE -8 + behavioral 50 (-> +4) = 56
        let score = apply_security_adjustments(60, -8, 50);
        assert_eq!(score, 56);
    }

    #[test]
    fn test_failed_tee_severely_penalizes() {
        // Base 60 + TEE -15 + behavioral 30 (-> 0) = 45
        let score = apply_security_adjustments(60, -15, 30);
        assert_eq!(score, 45);
    }

    #[test]
    fn test_score_clamped_to_100() {
        // Base 95 + TEE +8 + behavioral 90 (-> +10) = 113 -> clamped to 100
        let score = apply_security_adjustments(95, 8, 90);
        assert_eq!(score, 100);
    }

    #[test]
    fn test_score_clamped_to_0() {
        // Base 40 + TEE -15 + behavioral 0 (-> 0) = 25
        // WHY: This does NOT clamp to 0 because 25 > 0. The node is below 40
        // (consensus ineligible) but the raw score is still representable.
        let score = apply_security_adjustments(40, -15, 0);
        assert_eq!(score, 25);
    }

    #[test]
    fn test_score_clamped_to_0_extreme() {
        // Verify that truly negative results clamp to 0, not underflow.
        // Base 0 + TEE -15 + behavioral 0 (-> 0) = -15 -> clamped to 0
        let score = apply_security_adjustments(0, -15, 0);
        assert_eq!(score, 0);
    }

    #[test]
    fn test_behavioral_score_tiers() {
        // Tier boundary: 0-39 -> 0 bonus
        assert_eq!(apply_security_adjustments(60, 0, 0), 60);
        assert_eq!(apply_security_adjustments(60, 0, 39), 60);

        // Tier boundary: 40-59 -> 4 bonus
        assert_eq!(apply_security_adjustments(60, 0, 40), 64);
        assert_eq!(apply_security_adjustments(60, 0, 59), 64);

        // Tier boundary: 60-79 -> 7 bonus
        assert_eq!(apply_security_adjustments(60, 0, 60), 67);
        assert_eq!(apply_security_adjustments(60, 0, 79), 67);

        // Tier boundary: 80-100 -> 10 bonus
        assert_eq!(apply_security_adjustments(60, 0, 80), 70);
        assert_eq!(apply_security_adjustments(60, 0, 100), 70);
    }

    #[test]
    fn test_enhanced_score_breakdown() {
        let result = compute_enhanced_presence_score(60, 8, 85);

        assert_eq!(result.base_score, 60);
        assert_eq!(result.tee_adjustment, 8);
        assert_eq!(result.behavioral_bonus, 10);
        assert_eq!(result.behavioral_raw, 85);
        assert_eq!(result.final_score, 78);
    }

    #[test]
    fn test_enhanced_score_failed_tee_low_behavioral() {
        let result = compute_enhanced_presence_score(60, -15, 30);

        assert_eq!(result.base_score, 60);
        assert_eq!(result.tee_adjustment, -15);
        assert_eq!(result.behavioral_bonus, 0);
        assert_eq!(result.behavioral_raw, 30);
        assert_eq!(result.final_score, 45);
    }
}
