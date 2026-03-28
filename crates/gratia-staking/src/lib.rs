//! gratia-staking — Capped staking with overflow pool for the Gratia protocol.
//!
//! Implements the staking model described in CLAUDE.md:
//! - Minimum stake required to participate in mining (governance-adjustable)
//! - Per-node stake cap (e.g., 1,000 GRAT)
//! - Stake above cap flows to Network Security Pool
//! - Pool yield distributed to ALL active mining nodes proportionally
//! - Whales earn yield on full staked amount but consensus power is capped

pub mod error;
pub mod pool;
pub mod rewards;
pub mod slashing;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::config::StakingConfig;
use gratia_core::types::{Lux, NodeId, StakeInfo};

use crate::error::StakingError;
use crate::pool::NetworkSecurityPool;
use crate::slashing::{SlashResult, SlashingHistory};

/// Per-node staking record used internally by StakingManager.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeStake {
    /// Amount staked counting toward consensus (capped at per_node_cap).
    effective_stake: Lux,
    /// Amount overflowed to the Network Security Pool.
    overflow_amount: Lux,
    /// Total amount committed by this node (effective + overflow).
    total_committed: Lux,
    /// When the node first staked.
    staked_at: DateTime<Utc>,
    /// When an unstake request was made (for cooldown tracking).
    unstake_requested_at: Option<DateTime<Utc>>,
    /// Amount requested for unstaking (pending cooldown).
    unstake_pending: Lux,
}

/// Central manager for all staking operations.
///
/// Coordinates stake/unstake lifecycle, overflow pool interactions,
/// and integrates with the slashing subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingManager {
    config: StakingConfig,
    /// Per-node staking records.
    stakes: HashMap<NodeId, NodeStake>,
    /// The Network Security Pool that holds overflow stake.
    pool: NetworkSecurityPool,
    /// Per-node slashing histories.
    slashing_histories: HashMap<NodeId, SlashingHistory>,
    /// Set of permanently banned nodes.
    banned_nodes: HashMap<NodeId, DateTime<Utc>>,
    /// When the staking minimum was activated (None = still at genesis zero).
    /// WHY: Once the network crosses staking_activation_threshold miners,
    /// a 30-day grace period begins. After the grace period, the minimum
    /// stake is enforced. This timestamp records when activation occurred.
    staking_activated_at: Option<DateTime<Utc>>,
}

impl StakingManager {
    /// Create a new staking manager with the given configuration.
    pub fn new(config: StakingConfig) -> Self {
        Self {
            config,
            stakes: HashMap::new(),
            pool: NetworkSecurityPool::new(),
            slashing_histories: HashMap::new(),
            banned_nodes: HashMap::new(),
            staking_activated_at: None,
        }
    }

    /// Get a reference to the network security pool.
    pub fn pool(&self) -> &NetworkSecurityPool {
        &self.pool
    }

    /// Get a mutable reference to the network security pool.
    pub fn pool_mut(&mut self) -> &mut NetworkSecurityPool {
        &mut self.pool
    }

    /// Get the current staking configuration.
    pub fn config(&self) -> &StakingConfig {
        &self.config
    }

    /// Update the staking configuration (e.g., via governance).
    pub fn update_config(&mut self, config: StakingConfig) {
        self.config = config;
    }

    /// Stake tokens for a node.
    ///
    /// If the total committed amount exceeds the per-node cap, the excess
    /// automatically flows to the Network Security Pool.
    pub fn stake(
        &mut self,
        node_id: NodeId,
        amount: Lux,
        now: DateTime<Utc>,
    ) -> Result<StakeInfo, StakingError> {
        if amount == 0 {
            return Err(StakingError::InvalidAmount {
                reason: "stake amount must be greater than zero".into(),
            });
        }

        if self.banned_nodes.contains_key(&node_id) {
            return Err(StakingError::NodeBanned { node_id });
        }

        let entry = self.stakes.entry(node_id).or_insert(NodeStake {
            effective_stake: 0,
            overflow_amount: 0,
            total_committed: 0,
            staked_at: now,
            unstake_requested_at: None,
            unstake_pending: 0,
        });

        let new_total = entry.total_committed.checked_add(amount).ok_or_else(|| {
            StakingError::InvalidAmount {
                reason: "total committed stake would overflow u64".into(),
            }
        })?;

        entry.total_committed = new_total;

        // WHY: Recalculate effective vs overflow from scratch based on the new total.
        // This handles both initial staking and incremental top-ups correctly.
        let cap = self.config.per_node_cap;
        let new_effective = new_total.min(cap);
        let new_overflow = new_total.saturating_sub(cap);

        // If overflow increased, add the delta to the pool.
        let overflow_delta = new_overflow.saturating_sub(entry.overflow_amount);
        if overflow_delta > 0 {
            self.pool.add_overflow(node_id, overflow_delta, now)?;
        }

        entry.effective_stake = new_effective;
        entry.overflow_amount = new_overflow;

        Ok(Self::build_stake_info(&self.config, entry))
    }

    /// Unstake tokens from a node.
    ///
    /// Initiates an unstaking request subject to a cooldown period.
    /// Tokens are not immediately available — the cooldown must elapse first.
    /// Overflow is removed first, then effective stake.
    pub fn request_unstake(
        &mut self,
        node_id: NodeId,
        amount: Lux,
        now: DateTime<Utc>,
    ) -> Result<StakeInfo, StakingError> {
        if amount == 0 {
            return Err(StakingError::InvalidAmount {
                reason: "unstake amount must be greater than zero".into(),
            });
        }

        let entry = self.stakes.get_mut(&node_id).ok_or(StakingError::NodeNotFound {
            node_id,
        })?;

        // Check if there's already a pending unstake in cooldown.
        if let Some(requested_at) = entry.unstake_requested_at {
            let elapsed = (now - requested_at).num_seconds().max(0) as u64;
            if elapsed < self.config.unstake_cooldown_secs {
                return Err(StakingError::CooldownActive {
                    remaining_secs: self.config.unstake_cooldown_secs - elapsed,
                });
            }
        }

        if amount > entry.total_committed {
            return Err(StakingError::InsufficientStake {
                available: entry.total_committed,
                required: amount,
            });
        }

        // WHY: Remove from overflow first, then effective stake.
        // This preserves the node's consensus participation as long as possible.
        let from_overflow = amount.min(entry.overflow_amount);
        let from_effective = amount - from_overflow;

        if from_overflow > 0 {
            self.pool.remove_overflow(&node_id, from_overflow, now)?;
        }

        entry.overflow_amount -= from_overflow;
        entry.effective_stake -= from_effective;
        entry.total_committed -= amount;
        entry.unstake_requested_at = Some(now);
        entry.unstake_pending = amount;

        let info = Self::build_stake_info(&self.config, entry);

        // WHY: Clean up the entry if everything is withdrawn to avoid stale records.
        if entry.total_committed == 0 {
            self.stakes.remove(&node_id);
        }

        Ok(info)
    }

    /// Complete a pending unstake after the cooldown period has elapsed.
    ///
    /// Returns the amount of Lux that can be released back to the node's wallet.
    pub fn complete_unstake(
        &mut self,
        node_id: NodeId,
        now: DateTime<Utc>,
    ) -> Result<Lux, StakingError> {
        let entry = self.stakes.get_mut(&node_id).ok_or(StakingError::NodeNotFound {
            node_id,
        })?;

        let requested_at = entry.unstake_requested_at.ok_or(StakingError::InvalidAmount {
            reason: "no pending unstake request".into(),
        })?;

        let elapsed = (now - requested_at).num_seconds().max(0) as u64;
        if elapsed < self.config.unstake_cooldown_secs {
            return Err(StakingError::CooldownActive {
                remaining_secs: self.config.unstake_cooldown_secs - elapsed,
            });
        }

        let released = entry.unstake_pending;
        entry.unstake_pending = 0;
        entry.unstake_requested_at = None;

        Ok(released)
    }

    /// Check whether a node meets the minimum stake requirement for mining.
    /// Check the network size and activate the minimum stake if threshold is crossed.
    /// Call this periodically (e.g., once per block) with the current active miner count.
    ///
    /// Returns true if staking was just activated (first time crossing threshold).
    pub fn check_activation(&mut self, active_miners: u64, now: DateTime<Utc>) -> bool {
        if self.staking_activated_at.is_some() {
            return false; // Already activated.
        }
        if active_miners >= self.config.staking_activation_threshold {
            self.staking_activated_at = Some(now);
            tracing::info!(
                active_miners,
                threshold = self.config.staking_activation_threshold,
                grace_days = self.config.staking_activation_grace_secs / 86400,
                "Staking minimum activated — {}-day grace period begins",
                self.config.staking_activation_grace_secs / 86400
            );
            true
        } else {
            false
        }
    }

    /// Get the effective minimum stake right now, accounting for activation state.
    ///
    /// - Before activation: 0 (anyone can mine)
    /// - During grace period: 0 (existing miners have time to accumulate)
    /// - After grace period: activated_minimum_stake
    pub fn effective_minimum_stake(&self, now: DateTime<Utc>) -> Lux {
        match self.staking_activated_at {
            None => 0, // Not activated yet — genesis rules.
            Some(activated_at) => {
                let elapsed = now.signed_duration_since(activated_at);
                let grace = chrono::Duration::seconds(
                    self.config.staking_activation_grace_secs as i64,
                );
                if elapsed < grace {
                    0 // Still in grace period.
                } else {
                    self.config.activated_minimum_stake
                }
            }
        }
    }

    /// Whether staking has been activated (threshold crossed).
    pub fn is_staking_activated(&self) -> bool {
        self.staking_activated_at.is_some()
    }

    /// When staking was activated, if at all.
    pub fn staking_activated_at(&self) -> Option<DateTime<Utc>> {
        self.staking_activated_at
    }

    /// Whether a node meets the minimum stake requirement.
    /// Uses the static config.minimum_stake for backwards compatibility.
    /// For time-aware checks that respect the grace period, use
    /// `meets_minimum_stake_at(node_id, now)`.
    pub fn meets_minimum_stake(&self, node_id: &NodeId) -> bool {
        self.stakes
            .get(node_id)
            .map(|s| s.effective_stake >= self.config.minimum_stake)
            .unwrap_or(false)
    }

    /// Whether a node meets the minimum stake, accounting for activation
    /// threshold and grace period.
    pub fn meets_minimum_stake_at(&self, node_id: &NodeId, now: DateTime<Utc>) -> bool {
        let min = self.effective_minimum_stake(now);
        if min == 0 {
            return true; // No minimum enforced — anyone can mine.
        }
        self.stakes
            .get(node_id)
            .map(|s| s.effective_stake >= min)
            .unwrap_or(false)
    }

    /// Get the effective stake for a node (capped at per_node_cap).
    /// This is the stake that counts toward consensus power.
    pub fn effective_stake(&self, node_id: &NodeId) -> Lux {
        self.stakes
            .get(node_id)
            .map(|s| s.effective_stake)
            .unwrap_or(0)
    }

    /// Get the total committed stake for a node (effective + overflow).
    pub fn total_stake(&self, node_id: &NodeId) -> Lux {
        self.stakes
            .get(node_id)
            .map(|s| s.total_committed)
            .unwrap_or(0)
    }

    /// Get full stake info for a node.
    pub fn get_stake_info(&self, node_id: &NodeId) -> Option<StakeInfo> {
        self.stakes.get(node_id).map(|s| Self::build_stake_info(&self.config, s))
    }

    /// Get a node's slashing history.
    pub fn get_slashing_history(&self, node_id: &NodeId) -> Option<&SlashingHistory> {
        self.slashing_histories.get(node_id)
    }

    /// Apply a slashing event to a node.
    ///
    /// Reduces the node's stake according to the slash result, updates slashing history,
    /// and may ban the node if the slash is critical.
    pub fn apply_slash(
        &mut self,
        result: &SlashResult,
        now: DateTime<Utc>,
    ) -> Result<(), StakingError> {
        let node_id = result.event.node_id;

        let entry = self.stakes.get_mut(&node_id).ok_or(StakingError::NodeNotFound {
            node_id,
        })?;

        // Apply stake reduction.
        let stake_slash = result
            .event
            .amount_slashed
            .saturating_sub(result.overflow_slashed);
        entry.effective_stake = entry.effective_stake.saturating_sub(stake_slash);
        entry.total_committed = entry
            .total_committed
            .saturating_sub(result.event.amount_slashed);

        // Apply overflow reduction.
        if result.overflow_slashed > 0 {
            self.pool
                .remove_overflow(&node_id, result.overflow_slashed, now)?;
            entry.overflow_amount = entry.overflow_amount.saturating_sub(result.overflow_slashed);
        }

        // Update slashing history.
        let history = self
            .slashing_histories
            .entry(node_id)
            .or_insert_with(SlashingHistory::default);

        match result.event.severity {
            slashing::SlashingSeverity::Warning => history.warnings += 1,
            slashing::SlashingSeverity::Minor => history.minor_slashes += 1,
            slashing::SlashingSeverity::Major => history.major_slashes += 1,
            slashing::SlashingSeverity::Critical => {
                history.is_banned = true;
                self.banned_nodes.insert(node_id, now);
            }
        }
        history.events.push(result.event.clone());

        // Clean up if stake is fully drained.
        if entry.total_committed == 0 {
            self.stakes.remove(&node_id);
        }

        Ok(())
    }

    /// Check if a node is permanently banned.
    pub fn is_banned(&self, node_id: &NodeId) -> bool {
        self.banned_nodes.contains_key(node_id)
    }

    /// Get the total number of staking nodes.
    pub fn staker_count(&self) -> usize {
        self.stakes.len()
    }

    /// Get all node IDs that currently meet minimum stake requirements.
    pub fn eligible_miners(&self) -> Vec<NodeId> {
        self.stakes
            .iter()
            .filter(|(_, s)| s.effective_stake >= self.config.minimum_stake)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Build the public `StakeInfo` from an internal `NodeStake`.
    fn build_stake_info(config: &StakingConfig, stake: &NodeStake) -> StakeInfo {
        StakeInfo {
            node_stake: stake.effective_stake,
            overflow_amount: stake.overflow_amount,
            total_committed: stake.total_committed,
            staked_at: stake.staked_at,
            meets_minimum: stake.effective_stake >= config.minimum_stake,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slashing::{
        build_slashing_event, SlashingConfig, SlashingHistory, SlashingPillar, SlashingSeverity,
    };
    use gratia_core::types::LUX_PER_GRAT;

    fn test_node(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId(bytes)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn default_config() -> StakingConfig {
        StakingConfig::default()
    }

    fn manager() -> StakingManager {
        StakingManager::new(default_config())
    }

    // ========================================================================
    // Basic staking
    // ========================================================================

    #[test]
    fn test_stake_below_cap() {
        let mut mgr = manager();
        let node = test_node(1);
        let amount = 500 * LUX_PER_GRAT;

        let info = mgr.stake(node, amount, now()).unwrap();

        assert_eq!(info.node_stake, amount);
        assert_eq!(info.overflow_amount, 0);
        assert_eq!(info.total_committed, amount);
        assert!(info.meets_minimum);
    }

    #[test]
    fn test_stake_above_cap_overflows() {
        let mut mgr = manager();
        let node = test_node(1);
        let cap = mgr.config().per_node_cap;
        let amount = cap + 500 * LUX_PER_GRAT;

        let info = mgr.stake(node, amount, now()).unwrap();

        assert_eq!(info.node_stake, cap);
        assert_eq!(info.overflow_amount, 500 * LUX_PER_GRAT);
        assert_eq!(info.total_committed, amount);
        assert_eq!(mgr.pool().total_overflow(), 500 * LUX_PER_GRAT);
    }

    #[test]
    fn test_stake_incremental_crosses_cap() {
        let mut mgr = manager();
        let node = test_node(1);
        let cap = mgr.config().per_node_cap;

        // First stake: below cap.
        mgr.stake(node, cap - 100, now()).unwrap();
        assert_eq!(mgr.pool().total_overflow(), 0);

        // Second stake: pushes over cap.
        let info = mgr.stake(node, 300, now()).unwrap();
        assert_eq!(info.node_stake, cap);
        assert_eq!(info.overflow_amount, 200);
        assert_eq!(info.total_committed, cap + 200);
        assert_eq!(mgr.pool().total_overflow(), 200);
    }

    #[test]
    fn test_stake_zero_fails() {
        let mut mgr = manager();
        let result = mgr.stake(test_node(1), 0, now());
        assert!(result.is_err());
    }

    // ========================================================================
    // Minimum stake
    // ========================================================================

    #[test]
    fn test_meets_minimum_stake() {
        // WHY: Use a non-zero minimum to test enforcement logic.
        // Genesis default is 0 (zero-delay onboarding), but governance
        // can raise it later, so the check must still work.
        let mut config = default_config();
        config.minimum_stake = 100_000_000; // 100 GRAT
        let mut mgr = StakingManager::new(config);
        let node = test_node(1);
        let min = mgr.config().minimum_stake;

        assert!(!mgr.meets_minimum_stake(&node));

        mgr.stake(node, min, now()).unwrap();
        assert!(mgr.meets_minimum_stake(&node));
    }

    #[test]
    fn test_below_minimum_stake() {
        let mut config = default_config();
        config.minimum_stake = 100_000_000; // 100 GRAT
        let mut mgr = StakingManager::new(config);
        let node = test_node(1);
        let min = mgr.config().minimum_stake;

        mgr.stake(node, min - 1, now()).unwrap();
        assert!(!mgr.meets_minimum_stake(&node));
    }

    // ========================================================================
    // Unstaking
    // ========================================================================

    #[test]
    fn test_unstake_from_overflow_first() {
        let mut mgr = manager();
        let node = test_node(1);
        let cap = mgr.config().per_node_cap;

        mgr.stake(node, cap + 500, now()).unwrap();
        let info = mgr.request_unstake(node, 300, now()).unwrap();

        // Should have removed from overflow first.
        assert_eq!(info.overflow_amount, 200);
        assert_eq!(info.node_stake, cap);
    }

    #[test]
    fn test_unstake_exceeds_total() {
        let mut mgr = manager();
        let node = test_node(1);

        mgr.stake(node, 1000, now()).unwrap();
        let result = mgr.request_unstake(node, 2000, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_unstake_removes_entry_when_drained() {
        let mut mgr = manager();
        let node = test_node(1);

        mgr.stake(node, 1000, now()).unwrap();
        mgr.request_unstake(node, 1000, now()).unwrap();

        assert_eq!(mgr.staker_count(), 0);
        assert!(mgr.get_stake_info(&node).is_none());
    }

    #[test]
    fn test_complete_unstake_before_cooldown_fails() {
        let mut mgr = manager();
        let node = test_node(1);

        mgr.stake(node, 1000, now()).unwrap();
        mgr.request_unstake(node, 500, now()).unwrap();

        // Try to complete immediately — should fail.
        let result = mgr.complete_unstake(node, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_complete_unstake_after_cooldown() {
        let mut mgr = manager();
        let node = test_node(1);
        let ts = now();

        mgr.stake(node, 1000, ts).unwrap();
        mgr.request_unstake(node, 500, ts).unwrap();

        // Advance past cooldown (7 days).
        let after_cooldown = ts + chrono::Duration::seconds(
            mgr.config().unstake_cooldown_secs as i64 + 1,
        );

        let released = mgr.complete_unstake(node, after_cooldown).unwrap();
        assert_eq!(released, 500);
    }

    // ========================================================================
    // Effective vs total stake
    // ========================================================================

    #[test]
    fn test_effective_vs_total_stake() {
        let mut mgr = manager();
        let node = test_node(1);
        let cap = mgr.config().per_node_cap;

        mgr.stake(node, cap * 3, now()).unwrap();

        assert_eq!(mgr.effective_stake(&node), cap);
        assert_eq!(mgr.total_stake(&node), cap * 3);
    }

    // ========================================================================
    // Slashing integration
    // ========================================================================

    #[test]
    fn test_apply_minor_slash() {
        let mut mgr = manager();
        let node = test_node(1);
        let config = SlashingConfig::default();

        mgr.stake(node, 1_000_000, now()).unwrap();

        let result = build_slashing_event(
            node,
            SlashingPillar::ProofOfLife,
            SlashingSeverity::Minor,
            "suspicious pattern".into(),
            1_000_000,
            0,
            &SlashingHistory::default(),
            &config,
            now(),
            100,
        );

        mgr.apply_slash(&result, now()).unwrap();

        // 10% slash of 1,000,000 = 100,000. Remaining = 900,000.
        assert_eq!(mgr.effective_stake(&node), 900_000);
        assert_eq!(
            mgr.get_slashing_history(&node).unwrap().minor_slashes,
            1
        );
    }

    #[test]
    fn test_apply_critical_slash_bans_node() {
        let mut mgr = manager();
        let node = test_node(1);
        let config = SlashingConfig::default();

        mgr.stake(node, 1_000_000, now()).unwrap();

        let result = build_slashing_event(
            node,
            SlashingPillar::EnergyFraud,
            SlashingSeverity::Critical,
            "emulator detected".into(),
            1_000_000,
            0,
            &SlashingHistory::default(),
            &config,
            now(),
            200,
        );

        mgr.apply_slash(&result, now()).unwrap();

        assert!(mgr.is_banned(&node));
        // Node's stake should be fully drained and entry removed.
        assert_eq!(mgr.staker_count(), 0);
    }

    #[test]
    fn test_banned_node_cannot_stake() {
        let mut mgr = manager();
        let node = test_node(1);
        let config = SlashingConfig::default();

        mgr.stake(node, 1_000_000, now()).unwrap();

        let result = build_slashing_event(
            node,
            SlashingPillar::EnergyFraud,
            SlashingSeverity::Critical,
            "emulator".into(),
            1_000_000,
            0,
            &SlashingHistory::default(),
            &config,
            now(),
            200,
        );
        mgr.apply_slash(&result, now()).unwrap();

        // Try to stake again — should be rejected.
        let err = mgr.stake(node, 1_000_000, now());
        assert!(err.is_err());
    }

    // ========================================================================
    // Eligible miners
    // ========================================================================

    #[test]
    fn test_eligible_miners() {
        let mut config = default_config();
        config.minimum_stake = 100_000_000; // 100 GRAT
        let mut mgr = StakingManager::new(config);
        let min = mgr.config().minimum_stake;

        let node_a = test_node(1);
        let node_b = test_node(2);
        let node_c = test_node(3);

        mgr.stake(node_a, min, now()).unwrap();
        mgr.stake(node_b, min - 1, now()).unwrap(); // Below minimum.
        mgr.stake(node_c, min * 2, now()).unwrap();

        let eligible = mgr.eligible_miners();
        assert_eq!(eligible.len(), 2);
        assert!(eligible.contains(&node_a));
        assert!(!eligible.contains(&node_b));
        assert!(eligible.contains(&node_c));
    }

    // ========================================================================
    // Staking activation threshold
    // ========================================================================

    #[test]
    fn test_activation_not_triggered_below_threshold() {
        let mut mgr = manager();
        let ts = now();

        // 999 miners — below 1,000 threshold.
        assert!(!mgr.check_activation(999, ts));
        assert!(!mgr.is_staking_activated());
        assert_eq!(mgr.effective_minimum_stake(ts), 0);
    }

    #[test]
    fn test_activation_triggers_at_threshold() {
        let mut mgr = manager();
        let ts = now();

        assert!(mgr.check_activation(1_000, ts));
        assert!(mgr.is_staking_activated());
        assert_eq!(mgr.staking_activated_at(), Some(ts));
    }

    #[test]
    fn test_activation_only_fires_once() {
        let mut mgr = manager();
        let ts = now();

        assert!(mgr.check_activation(1_000, ts));
        // Second call should return false (already activated).
        assert!(!mgr.check_activation(2_000, ts));
    }

    #[test]
    fn test_grace_period_minimum_is_zero() {
        let mut mgr = manager();
        let ts = now();

        mgr.check_activation(1_000, ts);

        // During grace period (30 days), effective minimum is still 0.
        let during_grace = ts + chrono::Duration::days(15);
        assert_eq!(mgr.effective_minimum_stake(during_grace), 0);

        // Everyone can still mine during grace.
        let node = test_node(1);
        assert!(mgr.meets_minimum_stake_at(&node, during_grace));
    }

    #[test]
    fn test_after_grace_period_minimum_enforced() {
        let mut mgr = manager();
        let ts = now();

        mgr.check_activation(1_000, ts);

        // After 30-day grace period, minimum kicks in.
        let after_grace = ts + chrono::Duration::days(31);
        assert_eq!(
            mgr.effective_minimum_stake(after_grace),
            mgr.config().activated_minimum_stake
        );

        // Node with no stake can no longer mine.
        let node = test_node(1);
        assert!(!mgr.meets_minimum_stake_at(&node, after_grace));

        // Node with enough stake can mine.
        mgr.stake(node, mgr.config().activated_minimum_stake, ts).unwrap();
        assert!(mgr.meets_minimum_stake_at(&node, after_grace));
    }

    #[test]
    fn test_genesis_everyone_can_mine() {
        let mgr = manager();
        let ts = now();

        // Before activation, even nodes with 0 stake can mine.
        let node = test_node(1);
        assert!(mgr.meets_minimum_stake_at(&node, ts));
        assert_eq!(mgr.effective_minimum_stake(ts), 0);
    }
}
