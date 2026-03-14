//! Three-pillar slashing — penalizes nodes that violate consensus rules.
//!
//! Slashing conditions map to the three consensus security pillars:
//! 1. Proof of Life fraud (faked sensor data, spoofed attestations)
//! 2. Stake manipulation (double-signing, invalid staking transactions)
//! 3. Energy fraud (emulator detection, fake ARM attestation)
//!
//! Penalties are graduated: warning -> partial slash -> full slash + ban.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::types::{Lux, NodeId};

/// Which of the three consensus security pillars was violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlashingPillar {
    /// Proof of Life fraud — faked sensor data, spoofed behavioral attestations,
    /// or colluded attestation generation.
    ProofOfLife,
    /// Stake manipulation — double-signing blocks, submitting conflicting
    /// staking transactions, or exploiting overflow pool mechanics.
    StakeManipulation,
    /// Energy fraud — running on an emulator/VM instead of real ARM hardware,
    /// faking energy expenditure proofs, or bypassing thermal management.
    EnergyFraud,
}

/// Severity level of the offense, determining the penalty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SlashingSeverity {
    /// First offense or minor violation. Node receives a warning.
    /// No stake is slashed, but the event is recorded on-chain.
    Warning,
    /// Repeated or moderate violation. A portion of stake is slashed.
    Minor,
    /// Serious violation. A larger portion of stake is slashed and
    /// mining eligibility is temporarily paused.
    Major,
    /// Severe or repeated major violation. Full slash of effective stake
    /// and permanent ban from mining.
    Critical,
}

/// A record of a slashing event applied to a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashingEvent {
    /// The node that was slashed.
    pub node_id: NodeId,
    /// Which pillar was violated.
    pub pillar: SlashingPillar,
    /// Severity of the offense.
    pub severity: SlashingSeverity,
    /// Amount of stake slashed (in Lux). Zero for warnings.
    pub amount_slashed: Lux,
    /// Human-readable reason for the slash.
    pub reason: String,
    /// When the slashing event occurred.
    pub timestamp: DateTime<Utc>,
    /// Whether the node's mining eligibility was paused.
    pub mining_paused: bool,
    /// Whether the node was permanently banned.
    pub permanently_banned: bool,
    /// Block height at which this slashing was applied.
    pub block_height: u64,
}

/// The result of applying a slash to a node's stake.
#[derive(Debug, Clone)]
pub struct SlashResult {
    /// The slashing event record.
    pub event: SlashingEvent,
    /// Remaining node stake after the slash (effective stake, not overflow).
    pub remaining_stake: Lux,
    /// Amount removed from overflow pool (if any).
    pub overflow_slashed: Lux,
}

/// Penalty schedule configuration.
/// All percentages are in basis points (1 bps = 0.01%).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashingConfig {
    /// Percentage of effective stake slashed for minor offenses (bps).
    pub minor_slash_bps: u32,
    /// Percentage of effective stake slashed for major offenses (bps).
    pub major_slash_bps: u32,
    /// Percentage of effective stake slashed for critical offenses (bps).
    /// Typically 10000 (100%) for critical — full slash.
    pub critical_slash_bps: u32,
    /// Duration (seconds) that mining is paused after a major slash.
    pub major_pause_duration_secs: u64,
    /// Number of minor offenses before auto-escalating to major.
    pub minor_escalation_threshold: u32,
    /// Number of major offenses before auto-escalating to critical.
    pub major_escalation_threshold: u32,
}

impl Default for SlashingConfig {
    fn default() -> Self {
        Self {
            // WHY: 5% for minor — meaningful deterrent but not catastrophic for honest mistakes.
            minor_slash_bps: 500,
            // WHY: 25% for major — serious penalty that makes repeated cheating expensive.
            major_slash_bps: 2500,
            // WHY: 100% for critical — complete loss of stake. Nuclear option for provable fraud.
            critical_slash_bps: 10_000,
            // WHY: 7 days pause aligns with the unstaking cooldown period, preventing
            // a slashed node from immediately re-entering with new stake.
            major_pause_duration_secs: 7 * 24 * 3600,
            // WHY: 3 minor offenses before escalation gives benefit of the doubt
            // for edge cases (sensor glitches, network issues) while catching patterns.
            minor_escalation_threshold: 3,
            // WHY: 2 major offenses before permanent ban — at this point the node
            // has lost significant stake and continued behavior is clearly adversarial.
            major_escalation_threshold: 2,
        }
    }
}

/// Tracks a node's slashing history for escalation logic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlashingHistory {
    /// Count of warning events.
    pub warnings: u32,
    /// Count of minor slashing events.
    pub minor_slashes: u32,
    /// Count of major slashing events.
    pub major_slashes: u32,
    /// Whether this node has been permanently banned.
    pub is_banned: bool,
    /// All slashing events for this node (most recent first).
    pub events: Vec<SlashingEvent>,
}

impl SlashingHistory {
    /// Determine the effective severity after applying escalation rules.
    ///
    /// If a node accumulates too many offenses at a given level,
    /// the severity automatically escalates.
    pub fn effective_severity(
        &self,
        base_severity: SlashingSeverity,
        config: &SlashingConfig,
    ) -> SlashingSeverity {
        if self.is_banned {
            // WHY: A banned node should not accumulate further events, but if
            // one does arrive it is always critical.
            return SlashingSeverity::Critical;
        }

        match base_severity {
            SlashingSeverity::Warning => {
                // Warnings don't escalate on their own, but they're recorded.
                SlashingSeverity::Warning
            }
            SlashingSeverity::Minor => {
                if self.minor_slashes >= config.minor_escalation_threshold {
                    SlashingSeverity::Major
                } else {
                    SlashingSeverity::Minor
                }
            }
            SlashingSeverity::Major => {
                if self.major_slashes >= config.major_escalation_threshold {
                    SlashingSeverity::Critical
                } else {
                    SlashingSeverity::Major
                }
            }
            SlashingSeverity::Critical => SlashingSeverity::Critical,
        }
    }
}

/// Calculate the slash amount for a given severity and node stake.
///
/// Returns `(amount_from_stake, amount_from_overflow)`.
/// The slash is applied first to the node's effective stake, then to overflow if needed.
pub fn calculate_slash_amount(
    severity: SlashingSeverity,
    node_stake: Lux,
    overflow_amount: Lux,
    config: &SlashingConfig,
) -> (Lux, Lux) {
    let slash_bps = match severity {
        SlashingSeverity::Warning => return (0, 0),
        SlashingSeverity::Minor => config.minor_slash_bps,
        SlashingSeverity::Major => config.major_slash_bps,
        SlashingSeverity::Critical => config.critical_slash_bps,
    };

    let total_stake = node_stake.saturating_add(overflow_amount);
    // WHY: u128 intermediate to prevent overflow in multiplication.
    let total_slash = (total_stake as u128 * slash_bps as u128 / 10_000u128) as Lux;

    // WHY: Slash from the node's effective stake first, then overflow.
    // This means the node loses consensus-relevant stake before pool stake,
    // immediately reducing their ability to participate in block production.
    let from_stake = total_slash.min(node_stake);
    let from_overflow = total_slash.saturating_sub(from_stake).min(overflow_amount);

    (from_stake, from_overflow)
}

/// Build a slashing event from the violation details and apply escalation.
pub fn build_slashing_event(
    node_id: NodeId,
    pillar: SlashingPillar,
    base_severity: SlashingSeverity,
    reason: String,
    node_stake: Lux,
    overflow_amount: Lux,
    history: &SlashingHistory,
    config: &SlashingConfig,
    now: DateTime<Utc>,
    block_height: u64,
) -> SlashResult {
    let effective_severity = history.effective_severity(base_severity, config);
    let (stake_slash, overflow_slash) =
        calculate_slash_amount(effective_severity, node_stake, overflow_amount, config);

    let total_slashed = stake_slash.saturating_add(overflow_slash);
    let remaining_stake = node_stake.saturating_sub(stake_slash);

    let mining_paused = matches!(
        effective_severity,
        SlashingSeverity::Major | SlashingSeverity::Critical
    );
    let permanently_banned = effective_severity == SlashingSeverity::Critical;

    let event = SlashingEvent {
        node_id,
        pillar,
        severity: effective_severity,
        amount_slashed: total_slashed,
        reason,
        timestamp: now,
        mining_paused,
        permanently_banned,
        block_height,
    };

    SlashResult {
        event,
        remaining_stake,
        overflow_slashed: overflow_slash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId(bytes)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn default_config() -> SlashingConfig {
        SlashingConfig::default()
    }

    #[test]
    fn test_warning_no_slash() {
        let config = default_config();
        let (from_stake, from_overflow) =
            calculate_slash_amount(SlashingSeverity::Warning, 1_000_000, 500_000, &config);
        assert_eq!(from_stake, 0);
        assert_eq!(from_overflow, 0);
    }

    #[test]
    fn test_minor_slash_5_percent() {
        let config = default_config();
        let (from_stake, from_overflow) =
            calculate_slash_amount(SlashingSeverity::Minor, 1_000_000, 0, &config);
        // 5% of 1,000,000 = 50,000
        assert_eq!(from_stake, 50_000);
        assert_eq!(from_overflow, 0);
    }

    #[test]
    fn test_major_slash_25_percent() {
        let config = default_config();
        let (from_stake, from_overflow) =
            calculate_slash_amount(SlashingSeverity::Major, 1_000_000, 0, &config);
        // 25% of 1,000,000 = 250,000
        assert_eq!(from_stake, 250_000);
        assert_eq!(from_overflow, 0);
    }

    #[test]
    fn test_critical_slash_full() {
        let config = default_config();
        let (from_stake, from_overflow) =
            calculate_slash_amount(SlashingSeverity::Critical, 1_000_000, 500_000, &config);
        // 100% of total (1,500,000): 1,000,000 from stake, 500,000 from overflow
        assert_eq!(from_stake, 1_000_000);
        assert_eq!(from_overflow, 500_000);
    }

    #[test]
    fn test_slash_spills_to_overflow() {
        let config = default_config();
        // Major slash = 25% of total (200,000 + 800,000) = 250,000
        // Node stake = 200,000, so 200,000 from stake + 50,000 from overflow
        let (from_stake, from_overflow) =
            calculate_slash_amount(SlashingSeverity::Major, 200_000, 800_000, &config);
        assert_eq!(from_stake, 200_000);
        assert_eq!(from_overflow, 50_000);
    }

    #[test]
    fn test_escalation_minor_to_major() {
        let config = default_config();
        let history = SlashingHistory {
            minor_slashes: 3, // At threshold
            ..Default::default()
        };

        let severity = history.effective_severity(SlashingSeverity::Minor, &config);
        assert_eq!(severity, SlashingSeverity::Major);
    }

    #[test]
    fn test_escalation_major_to_critical() {
        let config = default_config();
        let history = SlashingHistory {
            major_slashes: 2, // At threshold
            ..Default::default()
        };

        let severity = history.effective_severity(SlashingSeverity::Major, &config);
        assert_eq!(severity, SlashingSeverity::Critical);
    }

    #[test]
    fn test_no_escalation_below_threshold() {
        let config = default_config();
        let history = SlashingHistory {
            minor_slashes: 1,
            ..Default::default()
        };

        let severity = history.effective_severity(SlashingSeverity::Minor, &config);
        assert_eq!(severity, SlashingSeverity::Minor);
    }

    #[test]
    fn test_banned_node_always_critical() {
        let config = default_config();
        let history = SlashingHistory {
            is_banned: true,
            ..Default::default()
        };

        let severity = history.effective_severity(SlashingSeverity::Warning, &config);
        assert_eq!(severity, SlashingSeverity::Critical);
    }

    #[test]
    fn test_build_slashing_event_warning() {
        let config = default_config();
        let history = SlashingHistory::default();

        let result = build_slashing_event(
            test_node(1),
            SlashingPillar::ProofOfLife,
            SlashingSeverity::Warning,
            "suspicious sensor pattern".into(),
            1_000_000,
            0,
            &history,
            &config,
            now(),
            100,
        );

        assert_eq!(result.event.severity, SlashingSeverity::Warning);
        assert_eq!(result.event.amount_slashed, 0);
        assert!(!result.event.mining_paused);
        assert!(!result.event.permanently_banned);
        assert_eq!(result.remaining_stake, 1_000_000);
    }

    #[test]
    fn test_build_slashing_event_critical_bans() {
        let config = default_config();
        let history = SlashingHistory::default();

        let result = build_slashing_event(
            test_node(1),
            SlashingPillar::EnergyFraud,
            SlashingSeverity::Critical,
            "emulator detected".into(),
            1_000_000,
            500_000,
            &history,
            &config,
            now(),
            200,
        );

        assert_eq!(result.event.severity, SlashingSeverity::Critical);
        assert!(result.event.mining_paused);
        assert!(result.event.permanently_banned);
        assert_eq!(result.remaining_stake, 0);
        assert_eq!(result.overflow_slashed, 500_000);
    }

    #[test]
    fn test_pillar_variants_exist() {
        // Verify all three pillars from the design doc are represented.
        let _ = SlashingPillar::ProofOfLife;
        let _ = SlashingPillar::StakeManipulation;
        let _ = SlashingPillar::EnergyFraud;
    }
}
