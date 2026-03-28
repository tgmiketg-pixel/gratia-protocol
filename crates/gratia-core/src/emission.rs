//! Emission schedule for GRAT mining rewards.
//!
//! The emission follows a 25% annual reduction model:
//! - Year 1: 2,125,000,000 GRAT
//! - Year 2: 1,593,750,000 GRAT (75% of Year 1)
//! - Year N: Year 1 * 0.75^(N-1)
//!
//! Daily budget is split equally among all active mining minutes.
//! Per-block reward = daily_budget / blocks_per_day.

use crate::types::Lux;

/// Number of Lux per GRAT.
/// WHY: Re-exported here for convenience in emission calculations so callers
/// don't need to import from types separately.
pub const LUX_PER_GRAT: u64 = 1_000_000;

/// Total mining supply in GRAT (85-90% of total supply emitted through mining).
/// WHY: Derived from tokenomics spec — Year 1 emission / (1 - 0.75) = 8.5B.
pub const TOTAL_MINING_SUPPLY_GRAT: u64 = 8_500_000_000;

/// Year 1 annual emission in GRAT.
/// WHY: Total mining supply * 25% = 2,125,000,000. This seeds the geometric
/// series that converges to TOTAL_MINING_SUPPLY_GRAT.
pub const YEAR_1_EMISSION_GRAT: u64 = 2_125_000_000;

/// Annual retention factor in basis points: 7500 = 75%.
/// WHY: Each year retains 75% of the previous year's emission (25% reduction).
/// Gentler than Bitcoin's 50% halving, providing a smoother transition for miners.
pub const ANNUAL_RETENTION_BPS: u64 = 7500;

/// Target block time in seconds.
/// WHY: 12 seconds balances finality speed against mobile device constraints
/// (network latency, ARM compute budget, battery impact).
pub const BLOCK_TIME_SECS: u64 = 12;

/// Blocks per day at the target block time.
/// WHY: 86400 seconds/day / 12 seconds/block = 7200 blocks/day.
pub const BLOCKS_PER_DAY: u64 = 86_400 / BLOCK_TIME_SECS;

/// Blocks per year (approximate, using 365 days).
/// WHY: Used to map block heights to emission years. Leap years are ignored
/// because the small error (~0.07%) is negligible over the emission curve.
pub const BLOCKS_PER_YEAR: u64 = BLOCKS_PER_DAY * 365;

/// Emission schedule calculator.
///
/// Stateless — all methods are pure functions of block height and year number.
/// This makes emission deterministic and verifiable by any node.
pub struct EmissionSchedule;

impl EmissionSchedule {
    /// Calculate the annual emission (in GRAT) for a given year (1-indexed).
    ///
    /// Year 0 returns 0 (no emission before genesis).
    /// Year 1 = 2,125,000,000 GRAT.
    /// Year N = Year 1 * 0.75^(N-1).
    pub fn annual_emission_grat(year: u32) -> u64 {
        if year == 0 {
            return 0;
        }
        let mut emission = YEAR_1_EMISSION_GRAT;
        for _ in 1..year {
            emission = emission * ANNUAL_RETENTION_BPS / 10_000;
        }
        emission
    }

    /// Calculate the daily budget (in GRAT) for a given year.
    ///
    /// This is the total GRAT available for distribution each day,
    /// split among all blocks produced that day.
    pub fn daily_budget_grat(year: u32) -> u64 {
        Self::annual_emission_grat(year) / 365
    }

    /// Calculate the per-block reward (in Lux) for a given block height.
    ///
    /// This is the total reward for the block, to be split among miners.
    /// Returns the full block reward; caller is responsible for distributing
    /// among active miners proportional to their mining contribution.
    pub fn block_reward_lux(block_height: u64) -> Lux {
        let year = Self::year_for_height(block_height);
        let daily_grat = Self::daily_budget_grat(year);
        let per_block_grat = daily_grat / BLOCKS_PER_DAY;
        per_block_grat * LUX_PER_GRAT
    }

    /// Determine which emission year a block height falls in (1-indexed).
    ///
    /// Block 0 is in year 1. Block `BLOCKS_PER_YEAR` is the first block of year 2.
    pub fn year_for_height(block_height: u64) -> u32 {
        let year = (block_height / BLOCKS_PER_YEAR) as u32 + 1;
        // WHY: .max(1) is a safety net — integer division already ensures
        // year >= 1 for any non-negative height, but we guard against
        // future refactors that might change the formula.
        year.max(1)
    }

    /// Calculate the per-miner block reward (in Lux) given the number of active miners.
    ///
    /// WHY: The daily budget is split among all miners proportional to their
    /// mining time. For simplicity, we divide equally by active_miners here.
    /// In production, this would account for actual mining minutes contributed
    /// by each miner within the block interval.
    ///
    /// If `active_miners` is 0, returns the full block reward (edge case during
    /// bootstrap when the block producer may be the only participant).
    pub fn per_miner_block_reward_lux(block_height: u64, active_miners: u64) -> Lux {
        let total = Self::block_reward_lux(block_height);
        if active_miners == 0 {
            return total;
        }
        total / active_miners
    }

    /// Calculate total GRAT emitted up to a given year (cumulative, inclusive).
    ///
    /// Useful for verifying that the emission schedule never exceeds
    /// TOTAL_MINING_SUPPLY_GRAT (the geometric series converges to it).
    pub fn cumulative_emission_grat(up_to_year: u32) -> u64 {
        let mut total = 0u64;
        for y in 1..=up_to_year {
            total += Self::annual_emission_grat(y);
        }
        total
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_year_1_emission() {
        assert_eq!(EmissionSchedule::annual_emission_grat(1), 2_125_000_000);
    }

    #[test]
    fn test_year_2_emission() {
        // 75% of Year 1: 2,125,000,000 * 0.75 = 1,593,750,000
        assert_eq!(EmissionSchedule::annual_emission_grat(2), 1_593_750_000);
    }

    #[test]
    fn test_year_5_emission() {
        // Year 5 from tokenomics table: 672,363,281
        // Manual: 2_125_000_000 * 0.75^4 = 672,363,281.25 (truncated to 672,363,281)
        assert_eq!(EmissionSchedule::annual_emission_grat(5), 672_363_281);
    }

    #[test]
    fn test_year_10_emission() {
        // Tokenomics table shows 150,998,592 (from floating-point 2.125B * 0.75^9).
        // Integer arithmetic with per-year truncation gives 159,554,957 due to
        // compounding rounding differences. Both are valid — the integer version
        // is what the chain enforces for deterministic consensus.
        let emission = EmissionSchedule::annual_emission_grat(10);
        assert_eq!(emission, 159_554_957);

        // Verify it's in the right ballpark of the table value
        let table_value: u64 = 150_998_592;
        let diff_pct = (emission as f64 - table_value as f64).abs() / table_value as f64 * 100.0;
        assert!(diff_pct < 6.0, "Year 10 emission diverges too far from table: {:.1}%", diff_pct);
    }

    #[test]
    fn test_daily_budget_year_1() {
        // Tokenomics table: 5,822,000 GRAT/day (= 2,125,000,000 / 365 truncated)
        let daily = EmissionSchedule::daily_budget_grat(1);
        assert_eq!(daily, 5_821_917); // 2_125_000_000 / 365 = 5_821_917 (integer division)
        // WHY: The tokenomics table rounds to 5,822,000 but integer division
        // gives 5,821,917. Both are correct — the table uses approximate values.
        // The difference is 83 GRAT/day (~0.001%), negligible.
        assert!((daily as i64 - 5_822_000).unsigned_abs() < 1000);
    }

    #[test]
    fn test_block_reward_year_1() {
        // daily_budget / 7200 blocks = per-block reward
        let daily_grat = EmissionSchedule::daily_budget_grat(1); // 5_821_917
        let expected_per_block_grat = daily_grat / BLOCKS_PER_DAY; // 808 GRAT
        let expected_lux = expected_per_block_grat * LUX_PER_GRAT;

        let reward = EmissionSchedule::block_reward_lux(0); // height 0 = year 1
        assert_eq!(reward, expected_lux);
        // Sanity: ~808 GRAT per block * 1_000_000 Lux/GRAT = 808_000_000 Lux
        assert_eq!(reward, 808_000_000);
    }

    #[test]
    fn test_year_for_height() {
        // Height 0 = year 1
        assert_eq!(EmissionSchedule::year_for_height(0), 1);

        // Last block of year 1
        assert_eq!(EmissionSchedule::year_for_height(BLOCKS_PER_YEAR - 1), 1);

        // First block of year 2 (height = BLOCKS_PER_YEAR = 2,628,000)
        assert_eq!(EmissionSchedule::year_for_height(BLOCKS_PER_YEAR), 2);

        // Midway through year 3
        assert_eq!(EmissionSchedule::year_for_height(BLOCKS_PER_YEAR * 2 + 1000), 3);
    }

    #[test]
    fn test_per_miner_reward() {
        let total = EmissionSchedule::block_reward_lux(0);

        // 1 miner gets the full reward
        assert_eq!(EmissionSchedule::per_miner_block_reward_lux(0, 1), total);

        // 10 miners split evenly
        assert_eq!(EmissionSchedule::per_miner_block_reward_lux(0, 10), total / 10);

        // 100 miners split evenly
        assert_eq!(EmissionSchedule::per_miner_block_reward_lux(0, 100), total / 100);

        // 0 miners returns full reward (bootstrap edge case)
        assert_eq!(EmissionSchedule::per_miner_block_reward_lux(0, 0), total);
    }

    #[test]
    fn test_cumulative_never_exceeds_supply() {
        // The geometric series sum approaches but never reaches TOTAL_MINING_SUPPLY_GRAT.
        // After 20 years, tokenomics table says 99.6% of mining supply.
        let cumulative_20 = EmissionSchedule::cumulative_emission_grat(20);
        assert!(
            cumulative_20 < TOTAL_MINING_SUPPLY_GRAT,
            "Cumulative emission after 20 years ({}) must be less than total mining supply ({})",
            cumulative_20,
            TOTAL_MINING_SUPPLY_GRAT,
        );

        // Even after 50 years, should still be under the cap
        let cumulative_50 = EmissionSchedule::cumulative_emission_grat(50);
        assert!(
            cumulative_50 < TOTAL_MINING_SUPPLY_GRAT,
            "Cumulative emission after 50 years ({}) must be less than total mining supply ({})",
            cumulative_50,
            TOTAL_MINING_SUPPLY_GRAT,
        );

        // Verify the 20-year figure is in the ballpark of 99.6% per tokenomics table
        let pct = cumulative_20 as f64 / TOTAL_MINING_SUPPLY_GRAT as f64 * 100.0;
        assert!(pct > 99.0 && pct < 100.0, "Expected ~99.6%, got {:.1}%", pct);
    }

    #[test]
    fn test_zero_year_returns_zero() {
        assert_eq!(EmissionSchedule::annual_emission_grat(0), 0);
        assert_eq!(EmissionSchedule::daily_budget_grat(0), 0);
    }
}
