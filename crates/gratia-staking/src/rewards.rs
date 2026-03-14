//! Mining reward distribution — flat-rate rewards with 25% annual emission reduction.
//!
//! Every minute of mining earns the same reward. No diminishing returns.
//! Block rewards decrease by 25% annually (gentler than Bitcoin's 50% halving).
//! Geographic equity: underserved regions earn elevated rewards.

use serde::{Deserialize, Serialize};

use gratia_core::config::RewardsConfig;
use gratia_core::types::{Lux, NodeId};

/// Tracks the emission schedule and distributes mining rewards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionSchedule {
    /// Genesis block reward (in Lux) — the initial block reward before any reductions.
    genesis_block_reward: Lux,
    /// Annual emission reduction in basis points (e.g., 2500 = 25%).
    annual_reduction_bps: u32,
    /// Number of blocks per year, used to determine epoch boundaries.
    /// Calculated from target block time.
    blocks_per_year: u64,
    /// Total Lux emitted across all epochs so far.
    total_emitted: Lux,
    /// Current block height (used to compute which emission year we're in).
    current_height: u64,
}

/// The result of distributing rewards for a single block.
#[derive(Debug, Clone)]
pub struct BlockRewardDistribution {
    /// Block height this distribution applies to.
    pub height: u64,
    /// Total reward for this block (before splitting among miners).
    pub total_reward: Lux,
    /// Per-miner reward amounts. All active miners receive the same base amount.
    pub miner_rewards: Vec<MinerReward>,
    /// Amount allocated to the Network Security Pool from this block's reward.
    pub pool_allocation: Lux,
}

/// Reward for a single miner from a single block.
#[derive(Debug, Clone)]
pub struct MinerReward {
    pub node_id: NodeId,
    /// Base reward (equal for all miners).
    pub base_reward: Lux,
    /// Geographic equity bonus (elevated for underserved regions).
    pub geographic_bonus: Lux,
    /// Total reward for this miner.
    pub total: Lux,
}

/// Geographic equity category for a node's region.
/// Underserved regions receive elevated rewards per the CLAUDE.md design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeographicTier {
    /// Well-served region — standard reward rate.
    Standard,
    /// Underserved region — receives elevated rewards (up to 1.5x per config).
    Underserved,
}

impl EmissionSchedule {
    /// Create a new emission schedule from the rewards config and network block time.
    pub fn new(config: &RewardsConfig, target_block_time_secs: u64) -> Self {
        // WHY: Calculate blocks per year from the target block time so emission
        // reduction timing adapts if governance changes the block time.
        let seconds_per_year: u64 = 365 * 24 * 3600;
        let blocks_per_year = seconds_per_year / target_block_time_secs.max(1);

        Self {
            genesis_block_reward: config.block_reward,
            annual_reduction_bps: config.annual_reduction_bps,
            blocks_per_year,
            total_emitted: 0,
            current_height: 0,
        }
    }

    /// The current emission year (0-indexed). Year 0 is the first year after genesis.
    pub fn current_year(&self) -> u64 {
        self.current_height / self.blocks_per_year
    }

    /// Total Lux emitted so far.
    pub fn total_emitted(&self) -> Lux {
        self.total_emitted
    }

    /// Current block height tracked by this schedule.
    pub fn current_height(&self) -> u64 {
        self.current_height
    }

    /// Calculate the block reward for a given block height.
    ///
    /// Applies 25% annual reduction compounding each year.
    /// Year 0: genesis_block_reward
    /// Year 1: genesis_block_reward * 0.75
    /// Year 2: genesis_block_reward * 0.75^2
    /// etc.
    pub fn calculate_block_reward(&self, height: u64) -> Lux {
        let year = height / self.blocks_per_year;
        let mut reward = self.genesis_block_reward;

        for _ in 0..year {
            // WHY: Apply reduction as (reward * (10000 - reduction_bps)) / 10000
            // to avoid floating point. 10000 bps = 100%.
            let reduction_factor = 10_000u64 - self.annual_reduction_bps as u64;
            reward = (reward as u128 * reduction_factor as u128 / 10_000u128) as Lux;

            // WHY: Floor at 1 Lux — never reduce to zero so mining always has some reward.
            if reward == 0 {
                reward = 1;
                break;
            }
        }

        reward
    }

    /// Distribute rewards for a block to a set of active miners.
    ///
    /// The block reward is split:
    /// - 90% equally among all active miners (flat rate — every miner earns the same).
    /// - 10% to the Network Security Pool (funds overflow pool yield distribution).
    ///
    /// Geographic equity bonuses are applied on top for underserved regions.
    pub fn distribute_rewards(
        &mut self,
        height: u64,
        active_miners: &[(NodeId, GeographicTier)],
        geographic_bonus_max_bps: u32,
    ) -> BlockRewardDistribution {
        let total_reward = self.calculate_block_reward(height);

        if active_miners.is_empty() {
            return BlockRewardDistribution {
                height,
                total_reward,
                miner_rewards: Vec::new(),
                pool_allocation: total_reward,
            };
        }

        // WHY: 90/10 split ensures the overflow pool has ongoing funding to distribute
        // yield to all active miners, making whale overflow beneficial to the whole network.
        let pool_share_bps: u64 = 1000; // 10% to pool
        let pool_allocation =
            (total_reward as u128 * pool_share_bps as u128 / 10_000u128) as Lux;
        let miner_total = total_reward - pool_allocation;

        let per_miner_base = miner_total / active_miners.len() as u64;

        let mut miner_rewards = Vec::with_capacity(active_miners.len());
        let mut total_distributed: Lux = pool_allocation;

        for &(node_id, tier) in active_miners {
            let geographic_bonus = match tier {
                GeographicTier::Standard => 0,
                GeographicTier::Underserved => {
                    // WHY: Bonus is calculated as a percentage of the base reward.
                    // geographic_bonus_max_bps of 15000 means up to 1.5x, so the bonus
                    // portion is (15000 - 10000) / 10000 = 0.5x of base.
                    let bonus_bps = geographic_bonus_max_bps.saturating_sub(10_000) as u64;
                    (per_miner_base as u128 * bonus_bps as u128 / 10_000u128) as Lux
                }
            };

            let total = per_miner_base.saturating_add(geographic_bonus);
            total_distributed = total_distributed.saturating_add(total);

            miner_rewards.push(MinerReward {
                node_id,
                base_reward: per_miner_base,
                geographic_bonus,
                total,
            });
        }

        // Update emission tracking.
        self.current_height = height;
        self.total_emitted = self.total_emitted.saturating_add(total_distributed);

        BlockRewardDistribution {
            height,
            total_reward,
            miner_rewards,
            pool_allocation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::types::LUX_PER_GRAT;

    fn test_node(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId(bytes)
    }

    fn default_config() -> RewardsConfig {
        RewardsConfig::default()
    }

    fn default_schedule() -> EmissionSchedule {
        // 4-second block time (default from NetworkConfig).
        EmissionSchedule::new(&default_config(), 4)
    }

    #[test]
    fn test_year_zero_reward() {
        let schedule = default_schedule();
        let reward = schedule.calculate_block_reward(0);
        assert_eq!(reward, 50 * LUX_PER_GRAT);
    }

    #[test]
    fn test_year_one_reduction() {
        let schedule = default_schedule();
        let blocks_per_year = schedule.blocks_per_year;

        // Year 1 reward should be 75% of genesis.
        let reward = schedule.calculate_block_reward(blocks_per_year);
        let expected = (50 * LUX_PER_GRAT as u128 * 7500 / 10000) as Lux;
        assert_eq!(reward, expected);
    }

    #[test]
    fn test_year_two_compound_reduction() {
        let schedule = default_schedule();
        let blocks_per_year = schedule.blocks_per_year;

        // Year 2 = 75% * 75% = 56.25% of genesis.
        let reward = schedule.calculate_block_reward(2 * blocks_per_year);
        // 50 GRAT * 0.75 * 0.75 = 28.125 GRAT
        let expected = (50u128 * LUX_PER_GRAT as u128 * 7500 / 10000 * 7500 / 10000) as Lux;
        assert_eq!(reward, expected);
    }

    #[test]
    fn test_reward_never_zero() {
        let schedule = default_schedule();
        // Even at a very large height, reward should be at least 1 Lux.
        let reward = schedule.calculate_block_reward(1_000_000_000);
        assert!(reward >= 1);
    }

    #[test]
    fn test_distribute_rewards_equal_split() {
        let mut schedule = default_schedule();
        let miners = vec![
            (test_node(1), GeographicTier::Standard),
            (test_node(2), GeographicTier::Standard),
        ];

        let dist = schedule.distribute_rewards(0, &miners, 15000);

        assert_eq!(dist.miner_rewards.len(), 2);
        // Both miners get the same base reward.
        assert_eq!(
            dist.miner_rewards[0].base_reward,
            dist.miner_rewards[1].base_reward
        );
        // No geographic bonus for standard tier.
        assert_eq!(dist.miner_rewards[0].geographic_bonus, 0);
        assert_eq!(dist.miner_rewards[1].geographic_bonus, 0);
        // Pool gets 10%.
        let expected_pool = (50 * LUX_PER_GRAT as u128 * 1000 / 10000) as Lux;
        assert_eq!(dist.pool_allocation, expected_pool);
    }

    #[test]
    fn test_distribute_rewards_geographic_bonus() {
        let mut schedule = default_schedule();
        let miners = vec![
            (test_node(1), GeographicTier::Standard),
            (test_node(2), GeographicTier::Underserved),
        ];

        let dist = schedule.distribute_rewards(0, &miners, 15000);

        let standard = &dist.miner_rewards[0];
        let underserved = &dist.miner_rewards[1];

        // Same base reward.
        assert_eq!(standard.base_reward, underserved.base_reward);
        // Underserved gets a bonus.
        assert_eq!(standard.geographic_bonus, 0);
        assert!(underserved.geographic_bonus > 0);
        // Underserved total > standard total.
        assert!(underserved.total > standard.total);

        // Geographic bonus should be 50% of base (15000 - 10000 = 5000 bps = 50%).
        let expected_bonus =
            (standard.base_reward as u128 * 5000 / 10000) as Lux;
        assert_eq!(underserved.geographic_bonus, expected_bonus);
    }

    #[test]
    fn test_distribute_no_miners() {
        let mut schedule = default_schedule();
        let dist = schedule.distribute_rewards(0, &[], 15000);

        assert!(dist.miner_rewards.is_empty());
        // Full reward goes to pool when no miners.
        assert_eq!(dist.pool_allocation, dist.total_reward);
    }

    #[test]
    fn test_emission_tracking() {
        let mut schedule = default_schedule();
        let miners = vec![(test_node(1), GeographicTier::Standard)];

        schedule.distribute_rewards(0, &miners, 15000);
        assert!(schedule.total_emitted() > 0);
        assert_eq!(schedule.current_height(), 0);

        schedule.distribute_rewards(1, &miners, 15000);
        assert_eq!(schedule.current_height(), 1);
    }

    #[test]
    fn test_current_year() {
        let schedule = default_schedule();
        assert_eq!(schedule.current_year(), 0);
    }
}
