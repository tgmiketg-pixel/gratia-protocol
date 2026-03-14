//! Network Security Pool — manages overflow stake from nodes exceeding the per-node cap.
//!
//! When a node stakes more than the per-node cap, the excess flows into the
//! Network Security Pool. The pool yield is distributed proportionally to ALL
//! active mining nodes, meaning whale stake subsidizes small miners by design.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::types::{Lux, NodeId};

use crate::error::StakingError;

/// Tracks overflow stake contributions and distributes pool yield.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSecurityPool {
    /// Total Lux held in the overflow pool across all contributors.
    total_overflow: Lux,
    /// Per-node overflow contributions.
    /// Only nodes whose total stake exceeds the per-node cap have entries here.
    contributions: HashMap<NodeId, OverflowContribution>,
    /// Accumulated yield available for distribution (from staking rewards, fees, etc.).
    accumulated_yield: Lux,
    /// Last time yield was distributed.
    last_distribution: Option<DateTime<Utc>>,
}

/// A single node's overflow contribution to the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverflowContribution {
    /// Amount of Lux this node has in the overflow pool.
    pub amount: Lux,
    /// When this node first contributed overflow.
    pub contributed_at: DateTime<Utc>,
    /// When the contribution was last updated.
    pub last_updated: DateTime<Utc>,
}

/// Result of a yield distribution to a single node.
#[derive(Debug, Clone)]
pub struct YieldShare {
    pub node_id: NodeId,
    /// Yield earned from the node's own overflow contribution (proportional to pool ownership).
    pub overflow_yield: Lux,
    /// Yield earned as an active miner (equal share of remaining pool yield).
    pub miner_yield: Lux,
    /// Total yield for this node.
    pub total_yield: Lux,
}

impl NetworkSecurityPool {
    /// Create an empty pool.
    pub fn new() -> Self {
        Self {
            total_overflow: 0,
            contributions: HashMap::new(),
            accumulated_yield: 0,
            last_distribution: None,
        }
    }

    /// Total Lux currently in the overflow pool.
    pub fn total_overflow(&self) -> Lux {
        self.total_overflow
    }

    /// Accumulated yield waiting to be distributed.
    pub fn accumulated_yield(&self) -> Lux {
        self.accumulated_yield
    }

    /// Number of nodes contributing to the pool.
    pub fn contributor_count(&self) -> usize {
        self.contributions.len()
    }

    /// Get a node's overflow contribution, if any.
    pub fn get_contribution(&self, node_id: &NodeId) -> Option<&OverflowContribution> {
        self.contributions.get(node_id)
    }

    /// Add overflow stake from a node that exceeded the per-node cap.
    ///
    /// Called when a node stakes beyond the cap. The excess amount is routed here.
    pub fn add_overflow(
        &mut self,
        node_id: NodeId,
        amount: Lux,
        now: DateTime<Utc>,
    ) -> Result<(), StakingError> {
        if amount == 0 {
            return Err(StakingError::InvalidAmount {
                reason: "overflow amount must be greater than zero".into(),
            });
        }

        self.total_overflow = self.total_overflow.checked_add(amount).ok_or_else(|| {
            StakingError::InvalidAmount {
                reason: "pool overflow would exceed u64 max".into(),
            }
        })?;

        let entry = self.contributions.entry(node_id).or_insert(OverflowContribution {
            amount: 0,
            contributed_at: now,
            last_updated: now,
        });
        entry.amount = entry.amount.checked_add(amount).ok_or_else(|| {
            StakingError::InvalidAmount {
                reason: "node overflow contribution would exceed u64 max".into(),
            }
        })?;
        entry.last_updated = now;

        Ok(())
    }

    /// Remove overflow stake for a node (partial or full withdrawal).
    ///
    /// Called when a node unstakes and needs to reclaim overflow funds.
    pub fn remove_overflow(
        &mut self,
        node_id: &NodeId,
        amount: Lux,
        now: DateTime<Utc>,
    ) -> Result<(), StakingError> {
        if amount == 0 {
            return Err(StakingError::InvalidAmount {
                reason: "removal amount must be greater than zero".into(),
            });
        }

        let entry = self.contributions.get_mut(node_id).ok_or_else(|| {
            StakingError::NodeNotFound {
                node_id: *node_id,
            }
        })?;

        if amount > entry.amount {
            return Err(StakingError::InsufficientOverflow {
                available: entry.amount,
                requested: amount,
            });
        }

        entry.amount -= amount;
        entry.last_updated = now;
        self.total_overflow -= amount;

        // WHY: Remove the entry entirely when drained to zero to keep the map clean
        // and avoid zero-amount ghost entries affecting contributor_count().
        if entry.amount == 0 {
            self.contributions.remove(node_id);
        }

        Ok(())
    }

    /// Add yield to the pool (e.g., from block rewards allocated to the pool).
    pub fn add_yield(&mut self, amount: Lux) {
        self.accumulated_yield = self.accumulated_yield.saturating_add(amount);
    }

    /// Calculate a single node's share of the pool yield.
    ///
    /// Yield is distributed proportionally based on each node's fraction of
    /// the total overflow pool. Nodes that contributed more overflow earn
    /// proportionally more yield on their overflow amount.
    ///
    /// Returns `None` if the node has no overflow contribution.
    pub fn calculate_yield_share(&self, node_id: &NodeId) -> Option<Lux> {
        let contribution = self.contributions.get(node_id)?;

        if self.total_overflow == 0 || self.accumulated_yield == 0 {
            return Some(0);
        }
        if contribution.amount == 0 {
            return Some(0);
        }

        // WHY: Use u128 intermediate to avoid overflow in multiplication before division.
        // A node's yield share = (node_overflow / total_overflow) * accumulated_yield.
        let share = (contribution.amount as u128)
            .checked_mul(self.accumulated_yield as u128)
            .map(|product| (product / self.total_overflow as u128) as Lux)
            .unwrap_or(0);

        Some(share)
    }

    /// Distribute the accumulated pool yield across all active mining nodes.
    ///
    /// The yield is split proportionally based on each node's overflow contribution.
    /// Every active miner receives their proportional share. Nodes without overflow
    /// contributions receive nothing from the overflow yield — but the overall staking
    /// reward system (in `rewards.rs`) ensures all active miners get base rewards.
    ///
    /// Returns the list of yield shares distributed.
    pub fn distribute_pool_yield(
        &mut self,
        active_miners: &[NodeId],
        now: DateTime<Utc>,
    ) -> Vec<YieldShare> {
        if self.accumulated_yield == 0 || active_miners.is_empty() {
            return Vec::new();
        }

        let yield_to_distribute = self.accumulated_yield;

        // WHY: Split the yield into two portions:
        // 50% distributed proportionally to overflow contributors (rewards whales for locking capital).
        // 50% distributed equally to ALL active miners (subsidizes small miners by design).
        // This ratio ensures whales earn yield on their full staked amount while
        // wealth concentration benefits the entire network.
        let contributor_portion = yield_to_distribute / 2;
        let miner_portion = yield_to_distribute - contributor_portion;

        // Equal share for every active miner from the miner portion.
        let per_miner_yield = if active_miners.is_empty() {
            0
        } else {
            miner_portion / active_miners.len() as u64
        };

        let mut shares = Vec::with_capacity(active_miners.len());
        let mut total_distributed: Lux = 0;

        for &miner in active_miners {
            let overflow_yield = if self.total_overflow > 0 {
                self.contributions
                    .get(&miner)
                    .map(|c| {
                        // WHY: u128 to avoid overflow in multiplication.
                        ((c.amount as u128 * contributor_portion as u128)
                            / self.total_overflow as u128) as Lux
                    })
                    .unwrap_or(0)
            } else {
                0
            };

            let total = overflow_yield.saturating_add(per_miner_yield);
            total_distributed = total_distributed.saturating_add(total);

            shares.push(YieldShare {
                node_id: miner,
                overflow_yield,
                miner_yield: per_miner_yield,
                total_yield: total,
            });
        }

        // WHY: Due to integer division rounding, total_distributed may be slightly less
        // than yield_to_distribute. The dust remains in accumulated_yield for the next round.
        self.accumulated_yield = yield_to_distribute.saturating_sub(total_distributed);
        self.last_distribution = Some(now);

        shares
    }
}

impl Default for NetworkSecurityPool {
    fn default() -> Self {
        Self::new()
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

    #[test]
    fn test_new_pool_is_empty() {
        let pool = NetworkSecurityPool::new();
        assert_eq!(pool.total_overflow(), 0);
        assert_eq!(pool.accumulated_yield(), 0);
        assert_eq!(pool.contributor_count(), 0);
    }

    #[test]
    fn test_add_overflow() {
        let mut pool = NetworkSecurityPool::new();
        let node = test_node(1);

        pool.add_overflow(node, 500_000, now()).unwrap();

        assert_eq!(pool.total_overflow(), 500_000);
        assert_eq!(pool.contributor_count(), 1);
        assert_eq!(pool.get_contribution(&node).unwrap().amount, 500_000);
    }

    #[test]
    fn test_add_overflow_accumulates() {
        let mut pool = NetworkSecurityPool::new();
        let node = test_node(1);

        pool.add_overflow(node, 300_000, now()).unwrap();
        pool.add_overflow(node, 200_000, now()).unwrap();

        assert_eq!(pool.total_overflow(), 500_000);
        assert_eq!(pool.contributor_count(), 1);
        assert_eq!(pool.get_contribution(&node).unwrap().amount, 500_000);
    }

    #[test]
    fn test_add_overflow_zero_amount_fails() {
        let mut pool = NetworkSecurityPool::new();
        let result = pool.add_overflow(test_node(1), 0, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_overflow_partial() {
        let mut pool = NetworkSecurityPool::new();
        let node = test_node(1);

        pool.add_overflow(node, 500_000, now()).unwrap();
        pool.remove_overflow(&node, 200_000, now()).unwrap();

        assert_eq!(pool.total_overflow(), 300_000);
        assert_eq!(pool.get_contribution(&node).unwrap().amount, 300_000);
    }

    #[test]
    fn test_remove_overflow_full_removes_entry() {
        let mut pool = NetworkSecurityPool::new();
        let node = test_node(1);

        pool.add_overflow(node, 500_000, now()).unwrap();
        pool.remove_overflow(&node, 500_000, now()).unwrap();

        assert_eq!(pool.total_overflow(), 0);
        assert_eq!(pool.contributor_count(), 0);
        assert!(pool.get_contribution(&node).is_none());
    }

    #[test]
    fn test_remove_overflow_exceeds_contribution() {
        let mut pool = NetworkSecurityPool::new();
        let node = test_node(1);

        pool.add_overflow(node, 500_000, now()).unwrap();
        let result = pool.remove_overflow(&node, 600_000, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_overflow_unknown_node() {
        let mut pool = NetworkSecurityPool::new();
        let result = pool.remove_overflow(&test_node(99), 100, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_calculate_yield_share_proportional() {
        let mut pool = NetworkSecurityPool::new();
        let node_a = test_node(1);
        let node_b = test_node(2);

        // Node A contributes 75%, Node B contributes 25%.
        pool.add_overflow(node_a, 750_000, now()).unwrap();
        pool.add_overflow(node_b, 250_000, now()).unwrap();
        pool.add_yield(1_000_000);

        let share_a = pool.calculate_yield_share(&node_a).unwrap();
        let share_b = pool.calculate_yield_share(&node_b).unwrap();

        assert_eq!(share_a, 750_000);
        assert_eq!(share_b, 250_000);
    }

    #[test]
    fn test_calculate_yield_share_no_contribution() {
        let mut pool = NetworkSecurityPool::new();
        let node = test_node(1);
        pool.add_yield(1_000_000);

        // Node with no contribution returns None.
        assert!(pool.calculate_yield_share(&node).is_none());
    }

    #[test]
    fn test_distribute_pool_yield_mixed() {
        let mut pool = NetworkSecurityPool::new();
        let whale = test_node(1);
        let small_miner = test_node(2);

        // Only the whale has overflow.
        pool.add_overflow(whale, 1_000_000, now()).unwrap();
        pool.add_yield(1_000_000);

        let active = vec![whale, small_miner];
        let shares = pool.distribute_pool_yield(&active, now());

        assert_eq!(shares.len(), 2);

        // Whale gets 100% of contributor portion (500k) + 50% of miner portion (250k) = 750k
        let whale_share = shares.iter().find(|s| s.node_id == whale).unwrap();
        assert_eq!(whale_share.overflow_yield, 500_000);
        assert_eq!(whale_share.miner_yield, 250_000);
        assert_eq!(whale_share.total_yield, 750_000);

        // Small miner gets 0% of contributor portion + 50% of miner portion = 250k
        let small_share = shares.iter().find(|s| s.node_id == small_miner).unwrap();
        assert_eq!(small_share.overflow_yield, 0);
        assert_eq!(small_share.miner_yield, 250_000);
        assert_eq!(small_share.total_yield, 250_000);
    }

    #[test]
    fn test_distribute_empty_yield() {
        let mut pool = NetworkSecurityPool::new();
        let shares = pool.distribute_pool_yield(&[test_node(1)], now());
        assert!(shares.is_empty());
    }

    #[test]
    fn test_distribute_no_miners() {
        let mut pool = NetworkSecurityPool::new();
        pool.add_yield(1_000_000);
        let shares = pool.distribute_pool_yield(&[], now());
        assert!(shares.is_empty());
        // Yield should remain undistributed.
        assert_eq!(pool.accumulated_yield(), 1_000_000);
    }
}
