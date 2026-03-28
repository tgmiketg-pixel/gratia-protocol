//! Progressive trust tier system for the Gratia protocol.
//!
//! New nodes start at `Unverified` with maximum scrutiny and progress through
//! trust tiers by maintaining consecutive daily Proof of Life attestations.
//! Trust determines committee/governance eligibility and verification frequency,
//! but NEVER affects mining rewards — those are flat at every tier.
//!
//! Tier progression:
//! - **Unverified** (Day 0): Maximum scrutiny, mining allowed
//! - **Provisional** (Day 1-6): High scrutiny, mining allowed
//! - **Establishing** (Day 7-29): Standard scrutiny, mining allowed
//! - **Established** (Day 30-89): Normal scrutiny, committee eligible
//! - **Trusted** (Day 90+): Normal scrutiny, committee + governance eligible
//!
//! Missing a single day of PoL resets consecutive days and drops the node
//! back to Unverified. This is intentionally harsh — trust must be earned
//! continuously, not accumulated once and banked.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Trust Tier
// ============================================================================

/// Progressive trust tier based on consecutive PoL history.
///
/// Ordered from lowest to highest trust so that comparisons (`>=`, `<`) work
/// naturally on the derived `Ord` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TrustTier {
    Unverified,
    Provisional,
    Establishing,
    Established,
    Trusted,
}

/// Consecutive-day thresholds for each tier transition.
/// WHY: These values align with the onboarding spec in the whitepaper:
/// Day 0 = Unverified, Day 1 = Provisional, Day 7 = Establishing,
/// Day 30 = Established, Day 90 = Trusted.
const PROVISIONAL_THRESHOLD: u64 = 1;
const ESTABLISHING_THRESHOLD: u64 = 7;
const ESTABLISHED_THRESHOLD: u64 = 30;
const TRUSTED_THRESHOLD: u64 = 90;

impl TrustTier {
    /// Determine the trust tier from a consecutive PoL day count.
    pub fn from_consecutive_days(days: u64) -> TrustTier {
        if days >= TRUSTED_THRESHOLD {
            TrustTier::Trusted
        } else if days >= ESTABLISHED_THRESHOLD {
            TrustTier::Established
        } else if days >= ESTABLISHING_THRESHOLD {
            TrustTier::Establishing
        } else if days >= PROVISIONAL_THRESHOLD {
            TrustTier::Provisional
        } else {
            TrustTier::Unverified
        }
    }

    /// Get the scrutiny level for this tier.
    pub fn scrutiny_level(&self) -> ScrutinyLevel {
        match self {
            TrustTier::Unverified => ScrutinyLevel::Maximum,
            TrustTier::Provisional => ScrutinyLevel::High,
            TrustTier::Establishing => ScrutinyLevel::Standard,
            TrustTier::Established => ScrutinyLevel::Normal,
            TrustTier::Trusted => ScrutinyLevel::Normal,
        }
    }

    /// Whether this tier is eligible for the 21-validator committee.
    ///
    /// WHY: Committee participation requires enough history to establish that
    /// the node is a genuine, persistent human participant — not a temporary
    /// Sybil node. 30 days is the minimum to demonstrate sustained presence.
    pub fn is_committee_eligible(&self) -> bool {
        matches!(self, TrustTier::Established | TrustTier::Trusted)
    }

    /// Whether this tier is eligible for governance (proposing + voting).
    ///
    /// WHY: Governance requires 90+ days per the spec. This prevents a wave
    /// of new accounts from flooding proposals or swaying votes before
    /// establishing genuine long-term participation.
    pub fn is_governance_eligible(&self) -> bool {
        matches!(self, TrustTier::Trusted)
    }

    /// Whether this tier is eligible for mining.
    ///
    /// WHY: Mining is allowed at ALL tiers per the zero-delay onboarding spec.
    /// A brand-new node mines on Day 0 — what changes is scrutiny, not access.
    pub fn is_mining_eligible(&self) -> bool {
        true
    }

    /// How many more consecutive days until the next tier upgrade.
    ///
    /// Returns `None` if already at the maximum tier (Trusted).
    pub fn days_until_next_tier(&self, current_days: u64) -> Option<u64> {
        let next_threshold = match self {
            TrustTier::Unverified => PROVISIONAL_THRESHOLD,
            TrustTier::Provisional => ESTABLISHING_THRESHOLD,
            TrustTier::Establishing => ESTABLISHED_THRESHOLD,
            TrustTier::Established => TRUSTED_THRESHOLD,
            TrustTier::Trusted => return None,
        };
        // WHY: Saturating subtraction guards against the edge case where
        // current_days somehow exceeds the threshold for the current tier
        // (shouldn't happen in normal flow, but defensive coding is cheap).
        Some(next_threshold.saturating_sub(current_days))
    }
}

// ============================================================================
// Scrutiny Level
// ============================================================================

/// Verification intensity applied to a node based on its trust tier.
///
/// Higher scrutiny means more frequent verification challenges and stricter
/// behavioral thresholds, making it harder for fake nodes to persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrutinyLevel {
    Maximum,
    High,
    Standard,
    Normal,
}

impl ScrutinyLevel {
    /// Multiplier for how frequently this node receives verification challenges.
    ///
    /// WHY: New nodes get challenged 4x more often than established ones.
    /// This front-loads verification cost on potential attackers while reducing
    /// overhead for proven participants. The cost asymmetry is the point —
    /// legitimate users endure high scrutiny briefly, attackers endure it forever.
    pub fn challenge_frequency_multiplier(&self) -> f64 {
        match self {
            ScrutinyLevel::Maximum => 4.0,
            ScrutinyLevel::High => 2.0,
            ScrutinyLevel::Standard => 1.0,
            ScrutinyLevel::Normal => 1.0,
        }
    }

    /// Multiplier for behavioral analysis thresholds (suspicion scores, etc.).
    ///
    /// WHY: A 1.5x multiplier at Maximum means the suspicious-pattern
    /// thresholds from `PolValidator` are 50% stricter for brand-new nodes.
    /// This catches borderline phone-farm signatures that would slip through
    /// at normal scrutiny levels.
    pub fn behavioral_threshold_multiplier(&self) -> f64 {
        match self {
            ScrutinyLevel::Maximum => 1.5,
            ScrutinyLevel::High => 1.25,
            ScrutinyLevel::Standard => 1.0,
            ScrutinyLevel::Normal => 1.0,
        }
    }
}

// ============================================================================
// Trust State
// ============================================================================

/// Tracks a node's progressive trust state over time.
///
/// This struct is persisted alongside the node's identity and updated daily
/// when PoL results are finalized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustState {
    /// Current trust tier (derived from `consecutive_pol_days`).
    tier: TrustTier,
    /// Consecutive days of valid PoL — resets to 0 on any missed day.
    consecutive_pol_days: u64,
    /// Total days of valid PoL ever produced (never resets).
    total_pol_days: u64,
    /// When the node first produced a valid PoL attestation.
    first_pol_date: Option<DateTime<Utc>>,
    /// When the node last produced a valid PoL attestation.
    last_pol_date: Option<DateTime<Utc>>,
    /// Number of times trust has been reset by slashing.
    /// WHY: Tracked so the protocol can apply escalating penalties for
    /// repeat offenders. A node that has been slashed 3 times is a very
    /// different risk profile than one slashed once.
    slashing_resets: u32,
}

impl TrustState {
    /// Create a new trust state for a brand-new node.
    pub fn new() -> Self {
        TrustState {
            tier: TrustTier::Unverified,
            consecutive_pol_days: 0,
            total_pol_days: 0,
            first_pol_date: None,
            last_pol_date: None,
            slashing_resets: 0,
        }
    }

    /// Record a valid PoL day and update the trust tier.
    ///
    /// Called once per day after `PolValidator` confirms the day's data passed.
    pub fn record_valid_pol(&mut self, date: DateTime<Utc>) {
        self.consecutive_pol_days += 1;
        self.total_pol_days += 1;

        if self.first_pol_date.is_none() {
            self.first_pol_date = Some(date);
        }
        self.last_pol_date = Some(date);

        self.tier = TrustTier::from_consecutive_days(self.consecutive_pol_days);
    }

    /// Record a missed PoL day — resets consecutive streak and drops to Unverified.
    ///
    /// WHY: The reset is intentionally harsh. Trust in Gratia is a continuous
    /// proof that a real human is using this phone every day. A gap in that
    /// proof means the phone may have changed hands, been compromised, or
    /// been part of a rotation in a phone farm. Resetting to Unverified with
    /// maximum scrutiny is the safe default.
    pub fn record_missed_pol(&mut self) {
        self.consecutive_pol_days = 0;
        self.tier = TrustTier::Unverified;
    }

    /// Reset trust due to a slashing event (detected fraud, failed challenge, etc.).
    ///
    /// WHY: Slashing is more severe than a missed day — it resets to Unverified
    /// regardless of history AND increments the slashing counter. The counter
    /// enables escalating responses: first slash = reset, second = longer
    /// probation, third = potential permanent ban (at the consensus layer's
    /// discretion).
    pub fn apply_slashing_reset(&mut self) {
        self.consecutive_pol_days = 0;
        self.tier = TrustTier::Unverified;
        self.slashing_resets += 1;
    }

    /// Get the current trust tier.
    pub fn tier(&self) -> TrustTier {
        self.tier
    }

    /// Get the current scrutiny level.
    pub fn scrutiny(&self) -> ScrutinyLevel {
        self.tier.scrutiny_level()
    }

    /// Get the consecutive PoL day count.
    pub fn consecutive_pol_days(&self) -> u64 {
        self.consecutive_pol_days
    }

    /// Get the total (lifetime) PoL day count.
    pub fn total_pol_days(&self) -> u64 {
        self.total_pol_days
    }

    /// Get the first PoL date, if any.
    pub fn first_pol_date(&self) -> Option<DateTime<Utc>> {
        self.first_pol_date
    }

    /// Get the last PoL date, if any.
    pub fn last_pol_date(&self) -> Option<DateTime<Utc>> {
        self.last_pol_date
    }

    /// Get the number of slashing resets.
    pub fn slashing_resets(&self) -> u32 {
        self.slashing_resets
    }
}

impl Default for TrustState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn test_new_node_is_unverified() {
        let state = TrustState::new();
        assert_eq!(state.tier(), TrustTier::Unverified);
        assert_eq!(state.consecutive_pol_days(), 0);
        assert_eq!(state.total_pol_days(), 0);
        assert!(state.first_pol_date().is_none());
        assert!(state.last_pol_date().is_none());
        assert_eq!(state.slashing_resets(), 0);
    }

    #[test]
    fn test_tier_progression() {
        let mut state = TrustState::new();
        let base = Utc::now();

        // Day 1 -> Provisional
        state.record_valid_pol(base);
        assert_eq!(state.tier(), TrustTier::Provisional);

        // Days 2-6 -> still Provisional
        for i in 1..7 {
            state.record_valid_pol(base + Duration::days(i));
        }
        assert_eq!(state.consecutive_pol_days(), 7);

        // Day 7 -> Establishing
        assert_eq!(state.tier(), TrustTier::Establishing);

        // Days 8-29 -> still Establishing
        for i in 7..30 {
            state.record_valid_pol(base + Duration::days(i));
        }
        assert_eq!(state.consecutive_pol_days(), 30);

        // Day 30 -> Established
        assert_eq!(state.tier(), TrustTier::Established);

        // Days 31-89 -> still Established
        for i in 30..90 {
            state.record_valid_pol(base + Duration::days(i));
        }
        assert_eq!(state.consecutive_pol_days(), 90);

        // Day 90 -> Trusted
        assert_eq!(state.tier(), TrustTier::Trusted);
    }

    #[test]
    fn test_missed_pol_resets_to_unverified() {
        let mut state = TrustState::new();
        let base = Utc::now();

        // Build up to Establishing (7 days)
        for i in 0..10 {
            state.record_valid_pol(base + Duration::days(i));
        }
        assert_eq!(state.tier(), TrustTier::Establishing);
        assert_eq!(state.total_pol_days(), 10);

        // Miss a day
        state.record_missed_pol();
        assert_eq!(state.tier(), TrustTier::Unverified);
        assert_eq!(state.consecutive_pol_days(), 0);
        // Total days are NOT reset
        assert_eq!(state.total_pol_days(), 10);
    }

    #[test]
    fn test_slashing_reset() {
        let mut state = TrustState::new();
        let base = Utc::now();

        // Build up to Established
        for i in 0..35 {
            state.record_valid_pol(base + Duration::days(i));
        }
        assert_eq!(state.tier(), TrustTier::Established);

        // Slashing resets everything
        state.apply_slashing_reset();
        assert_eq!(state.tier(), TrustTier::Unverified);
        assert_eq!(state.consecutive_pol_days(), 0);
        assert_eq!(state.slashing_resets(), 1);
        // Total days preserved
        assert_eq!(state.total_pol_days(), 35);
    }

    #[test]
    fn test_committee_eligibility_per_tier() {
        assert!(!TrustTier::Unverified.is_committee_eligible());
        assert!(!TrustTier::Provisional.is_committee_eligible());
        assert!(!TrustTier::Establishing.is_committee_eligible());
        assert!(TrustTier::Established.is_committee_eligible());
        assert!(TrustTier::Trusted.is_committee_eligible());
    }

    #[test]
    fn test_governance_eligibility_per_tier() {
        assert!(!TrustTier::Unverified.is_governance_eligible());
        assert!(!TrustTier::Provisional.is_governance_eligible());
        assert!(!TrustTier::Establishing.is_governance_eligible());
        assert!(!TrustTier::Established.is_governance_eligible());
        assert!(TrustTier::Trusted.is_governance_eligible());
    }

    #[test]
    fn test_mining_always_eligible() {
        assert!(TrustTier::Unverified.is_mining_eligible());
        assert!(TrustTier::Provisional.is_mining_eligible());
        assert!(TrustTier::Establishing.is_mining_eligible());
        assert!(TrustTier::Established.is_mining_eligible());
        assert!(TrustTier::Trusted.is_mining_eligible());
    }

    #[test]
    fn test_scrutiny_levels_per_tier() {
        assert_eq!(TrustTier::Unverified.scrutiny_level(), ScrutinyLevel::Maximum);
        assert_eq!(TrustTier::Provisional.scrutiny_level(), ScrutinyLevel::High);
        assert_eq!(TrustTier::Establishing.scrutiny_level(), ScrutinyLevel::Standard);
        assert_eq!(TrustTier::Established.scrutiny_level(), ScrutinyLevel::Normal);
        assert_eq!(TrustTier::Trusted.scrutiny_level(), ScrutinyLevel::Normal);

        // Verify multiplier values
        assert_eq!(ScrutinyLevel::Maximum.challenge_frequency_multiplier(), 4.0);
        assert_eq!(ScrutinyLevel::High.challenge_frequency_multiplier(), 2.0);
        assert_eq!(ScrutinyLevel::Standard.challenge_frequency_multiplier(), 1.0);
        assert_eq!(ScrutinyLevel::Normal.challenge_frequency_multiplier(), 1.0);

        assert_eq!(ScrutinyLevel::Maximum.behavioral_threshold_multiplier(), 1.5);
        assert_eq!(ScrutinyLevel::High.behavioral_threshold_multiplier(), 1.25);
        assert_eq!(ScrutinyLevel::Standard.behavioral_threshold_multiplier(), 1.0);
        assert_eq!(ScrutinyLevel::Normal.behavioral_threshold_multiplier(), 1.0);
    }

    #[test]
    fn test_days_until_next_tier() {
        // Unverified at day 0 -> 1 day to Provisional
        assert_eq!(TrustTier::Unverified.days_until_next_tier(0), Some(1));

        // Provisional at day 3 -> 4 days to Establishing (threshold = 7)
        assert_eq!(TrustTier::Provisional.days_until_next_tier(3), Some(4));

        // Establishing at day 7 -> 23 days to Established (threshold = 30)
        assert_eq!(TrustTier::Establishing.days_until_next_tier(7), Some(23));

        // Establishing at day 20 -> 10 days to Established
        assert_eq!(TrustTier::Establishing.days_until_next_tier(20), Some(10));

        // Established at day 30 -> 60 days to Trusted (threshold = 90)
        assert_eq!(TrustTier::Established.days_until_next_tier(30), Some(60));

        // Trusted -> None (already at max)
        assert_eq!(TrustTier::Trusted.days_until_next_tier(100), None);
    }

    #[test]
    fn test_multiple_slashing_resets_tracked() {
        let mut state = TrustState::new();
        let base = Utc::now();

        // Build up, slash, rebuild, slash again
        for i in 0..10 {
            state.record_valid_pol(base + Duration::days(i));
        }
        state.apply_slashing_reset();
        assert_eq!(state.slashing_resets(), 1);

        for i in 10..20 {
            state.record_valid_pol(base + Duration::days(i));
        }
        state.apply_slashing_reset();
        assert_eq!(state.slashing_resets(), 2);

        state.apply_slashing_reset();
        assert_eq!(state.slashing_resets(), 3);

        // Total days still reflect all valid days ever recorded
        assert_eq!(state.total_pol_days(), 20);
        assert_eq!(state.consecutive_pol_days(), 0);
        assert_eq!(state.tier(), TrustTier::Unverified);
    }

    #[test]
    fn test_first_and_last_pol_dates() {
        let mut state = TrustState::new();
        let day1 = Utc::now();
        let day5 = day1 + Duration::days(4);

        state.record_valid_pol(day1);
        assert_eq!(state.first_pol_date(), Some(day1));
        assert_eq!(state.last_pol_date(), Some(day1));

        for i in 1..5 {
            state.record_valid_pol(day1 + Duration::days(i));
        }
        // First date unchanged, last date updated
        assert_eq!(state.first_pol_date(), Some(day1));
        assert_eq!(state.last_pol_date(), Some(day5));
    }

    #[test]
    fn test_rebuild_after_missed_day() {
        let mut state = TrustState::new();
        let base = Utc::now();

        // Build to Establishing
        for i in 0..10 {
            state.record_valid_pol(base + Duration::days(i));
        }
        assert_eq!(state.tier(), TrustTier::Establishing);

        // Miss a day
        state.record_missed_pol();
        assert_eq!(state.tier(), TrustTier::Unverified);

        // Rebuild — starts from scratch for consecutive days
        state.record_valid_pol(base + Duration::days(11));
        assert_eq!(state.tier(), TrustTier::Provisional);
        assert_eq!(state.consecutive_pol_days(), 1);
        // But total keeps accumulating
        assert_eq!(state.total_pol_days(), 11);
    }

    #[test]
    fn test_tier_ord_ordering() {
        // Verify that the derived Ord gives the expected ordering
        assert!(TrustTier::Unverified < TrustTier::Provisional);
        assert!(TrustTier::Provisional < TrustTier::Establishing);
        assert!(TrustTier::Establishing < TrustTier::Established);
        assert!(TrustTier::Established < TrustTier::Trusted);
    }
}
