//! Stake Manipulation Tests
//!
//! Tests staking edge cases and attack vectors:
//! - Staking exactly at the per-node cap
//! - Staking above cap — overflow to Network Security Pool
//! - Rapid stake/unstake gaming — cooldown period prevents it
//! - Pool yield distribution — proportional to all active miners
//! - Zero stake — cannot mine (minimum not met)
//! - Whale stakes 1M GRAT — excess subsidizes small miners via pool

use chrono::{Duration, Utc};
use gratia_core::config::StakingConfig;
use gratia_core::types::{Lux, NodeId, LUX_PER_GRAT};
use gratia_staking::StakingManager;
use gratia_staking::rewards::{EmissionSchedule, GeographicTier};
use gratia_staking::slashing::{
    SlashingConfig, SlashingHistory, SlashingPillar, SlashingSeverity, build_slashing_event,
};

// ============================================================================
// Helpers
// ============================================================================

fn test_node(id: u8) -> NodeId {
    let mut bytes = [0u8; 32];
    bytes[0] = id;
    NodeId(bytes)
}

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn default_config() -> StakingConfig {
    StakingConfig::default()
}

// ============================================================================
// Tests
// ============================================================================

/// Staking exactly at cap: no overflow, full consensus weight.
#[test]
fn test_stake_exactly_at_cap() {
    let config = default_config();
    let cap = config.per_node_cap;
    let mut mgr = StakingManager::new(config);
    let node = test_node(1);

    let info = mgr.stake(node, cap, now()).unwrap();

    assert_eq!(info.node_stake, cap, "Effective stake should equal cap");
    assert_eq!(info.overflow_amount, 0, "No overflow at exactly cap");
    assert_eq!(info.total_committed, cap);
    assert!(info.meets_minimum);
    assert_eq!(mgr.pool().total_overflow(), 0);
}

/// Staking above cap: overflow goes to pool, consensus weight capped.
#[test]
fn test_stake_above_cap_overflows_to_pool() {
    let config = default_config();
    let cap = config.per_node_cap;
    let excess = 500 * LUX_PER_GRAT;
    let mut mgr = StakingManager::new(config);
    let node = test_node(1);

    let info = mgr.stake(node, cap + excess, now()).unwrap();

    assert_eq!(info.node_stake, cap, "Effective stake capped");
    assert_eq!(info.overflow_amount, excess, "Excess should overflow");
    assert_eq!(info.total_committed, cap + excess);
    assert_eq!(mgr.pool().total_overflow(), excess);
    assert_eq!(mgr.pool().contributor_count(), 1);
}

/// ATTACK: Rapidly stake/unstake to game the system.
/// DEFENSE: Cooldown period prevents immediate re-staking after unstake.
#[test]
fn test_rapid_stake_unstake_cooldown_prevents_gaming() {
    let config = default_config();
    let mut mgr = StakingManager::new(config.clone());
    let node = test_node(1);
    let ts = now();

    mgr.stake(node, 1000 * LUX_PER_GRAT, ts).unwrap();

    // First unstake request.
    mgr.request_unstake(node, 500 * LUX_PER_GRAT, ts).unwrap();

    // Try to unstake again immediately — should fail (cooldown active).
    let result = mgr.request_unstake(node, 100 * LUX_PER_GRAT, ts);
    assert!(
        result.is_err(),
        "Second unstake during cooldown should fail"
    );

    // Try to complete the unstake before cooldown — should fail.
    let result = mgr.complete_unstake(node, ts);
    assert!(
        result.is_err(),
        "Completing unstake before cooldown should fail"
    );

    // Advance past the cooldown period (7 days).
    let after_cooldown = ts + Duration::seconds(config.unstake_cooldown_secs as i64 + 1);
    let released = mgr.complete_unstake(node, after_cooldown).unwrap();
    assert_eq!(released, 500 * LUX_PER_GRAT);
}

/// Pool yield distribution: proportional to all active miners.
/// WHY: Whale overflow subsidizes small miners by design.
#[test]
fn test_pool_yield_distribution_proportional() {
    let config = default_config();
    let cap = config.per_node_cap;
    let mut mgr = StakingManager::new(config);
    let ts = now();

    // Whale: stakes 5x cap (4x goes to overflow).
    let whale = test_node(1);
    mgr.stake(whale, cap * 5, ts).unwrap();

    // Small miner: stakes exactly at minimum.
    let small = test_node(2);
    mgr.stake(small, mgr.config().minimum_stake, ts).unwrap();

    // Add yield to the pool.
    mgr.pool_mut().add_yield(1_000_000);

    // Distribute pool yield to both active miners.
    let active = vec![whale, small];
    let shares = mgr.pool_mut().distribute_pool_yield(&active, ts);

    assert_eq!(shares.len(), 2);

    let whale_share = shares.iter().find(|s| s.node_id == whale).unwrap();
    let small_share = shares.iter().find(|s| s.node_id == small).unwrap();

    // WHY: Both miners get the miner portion (50% split equally).
    assert!(
        small_share.miner_yield > 0,
        "Small miner should get miner yield"
    );
    assert_eq!(
        whale_share.miner_yield, small_share.miner_yield,
        "Miner portion should be equal"
    );

    // Whale also gets overflow yield from the contributor portion.
    assert!(
        whale_share.overflow_yield > 0,
        "Whale should get overflow yield"
    );
    assert_eq!(
        small_share.overflow_yield, 0,
        "Small miner has no overflow contribution"
    );

    // Small miner benefits from the system — they get yield they wouldn't
    // have access to without whale participation.
    assert!(
        small_share.total_yield > 0,
        "Small miner should benefit from pool"
    );
}

/// Zero stake: cannot mine (minimum not met).
#[test]
fn test_zero_stake_cannot_mine() {
    let config = default_config();
    let mgr = StakingManager::new(config);
    let node = test_node(1);

    assert!(
        !mgr.meets_minimum_stake(&node),
        "Node with zero stake should not meet minimum"
    );
    assert_eq!(mgr.effective_stake(&node), 0);
    assert!(mgr.eligible_miners().is_empty());
}

/// ATTACK: Whale stakes 1M GRAT — excess subsidizes small miners via pool.
/// VERIFY: Consensus power is capped, but yield benefits entire network.
#[test]
fn test_whale_stakes_1m_grat_excess_subsidizes_network() {
    let config = default_config();
    let cap = config.per_node_cap;
    let mut mgr = StakingManager::new(config.clone());
    let ts = now();

    let whale = test_node(1);
    let whale_stake = 1_000_000 * LUX_PER_GRAT; // 1M GRAT

    mgr.stake(whale, whale_stake, ts).unwrap();

    // Effective (consensus) stake is capped.
    assert_eq!(mgr.effective_stake(&whale), cap);

    // Everything above cap is in the overflow pool.
    let expected_overflow = whale_stake - cap;
    assert_eq!(mgr.pool().total_overflow(), expected_overflow);

    // Total committed is the full amount.
    assert_eq!(mgr.total_stake(&whale), whale_stake);

    // Now add 10 small miners at minimum stake.
    for i in 2..12u8 {
        mgr.stake(test_node(i), config.minimum_stake, ts).unwrap();
    }

    // Add significant yield to the pool.
    mgr.pool_mut().add_yield(10_000 * LUX_PER_GRAT);

    // Distribute to all active miners.
    let active: Vec<NodeId> = (1..12u8).map(test_node).collect();
    let shares = mgr.pool_mut().distribute_pool_yield(&active, ts);

    // Every active miner (including small ones) gets yield.
    for share in &shares {
        assert!(
            share.total_yield > 0,
            "Miner {:?} should receive yield",
            share.node_id
        );
    }

    // Small miners collectively benefit from whale's overflow.
    let total_small_yield: Lux = shares
        .iter()
        .filter(|s| s.node_id != whale)
        .map(|s| s.total_yield)
        .sum();
    assert!(
        total_small_yield > 0,
        "Small miners should collectively receive yield from whale overflow"
    );
}

/// Staking below minimum: can deposit but not mine.
#[test]
fn test_below_minimum_stake_deposits_but_cannot_mine() {
    let config = default_config();
    let minimum = config.minimum_stake;
    let mut mgr = StakingManager::new(config);
    let node = test_node(1);

    mgr.stake(node, minimum - 1, now()).unwrap();

    // Stake exists but doesn't meet minimum.
    assert_eq!(mgr.effective_stake(&node), minimum - 1);
    assert!(!mgr.meets_minimum_stake(&node));
    assert!(mgr.eligible_miners().is_empty());
}

/// Unstake removes from overflow first, preserving consensus participation.
#[test]
fn test_unstake_preserves_consensus_stake() {
    let config = default_config();
    let cap = config.per_node_cap;
    let mut mgr = StakingManager::new(config);
    let node = test_node(1);

    // Stake 2x cap.
    mgr.stake(node, cap * 2, now()).unwrap();
    assert_eq!(mgr.effective_stake(&node), cap);
    assert_eq!(mgr.pool().total_overflow(), cap);

    // Unstake an amount smaller than overflow.
    mgr.request_unstake(node, cap / 2, now()).unwrap();

    // Consensus stake should be preserved — only overflow reduced.
    assert_eq!(
        mgr.effective_stake(&node),
        cap,
        "Consensus stake should remain at cap"
    );
    assert_eq!(
        mgr.pool().total_overflow(),
        cap / 2,
        "Overflow should be reduced"
    );
}

/// ATTACK: Slashing depletes stake — verify behavior when fully drained.
#[test]
fn test_slashing_fully_drains_stake() {
    let staking_config = default_config();
    let slash_config = SlashingConfig::default();
    let mut mgr = StakingManager::new(staking_config);
    let node = test_node(1);

    mgr.stake(node, 1_000 * LUX_PER_GRAT, now()).unwrap();

    // Critical slash: 100% of stake burned.
    let result = build_slashing_event(
        node,
        SlashingPillar::EnergyFraud,
        SlashingSeverity::Critical,
        "emulator detected".into(),
        mgr.effective_stake(&node),
        0,
        &SlashingHistory::default(),
        &slash_config,
        now(),
        100,
    );

    mgr.apply_slash(&result, now()).unwrap();

    // Node should be banned and have zero stake.
    assert!(mgr.is_banned(&node));
    assert_eq!(mgr.effective_stake(&node), 0);
    assert_eq!(mgr.staker_count(), 0);

    // Banned node cannot re-stake.
    let err = mgr.stake(node, 1_000 * LUX_PER_GRAT, now());
    assert!(err.is_err(), "Banned node should not be able to stake");
}

/// Progressive slashing: repeated minor offenses escalate to major.
#[test]
fn test_progressive_slashing_escalation() {
    let slash_config = SlashingConfig::default();

    let mut history = SlashingHistory::default();

    // First 2 minor offenses stay minor.
    for _ in 0..2 {
        let severity = history.effective_severity(SlashingSeverity::Minor, &slash_config);
        assert_eq!(severity, SlashingSeverity::Minor);
        history.minor_slashes += 1;
    }

    // 3rd minor offense escalates to major (threshold = 3).
    history.minor_slashes = 3;
    let severity = history.effective_severity(SlashingSeverity::Minor, &slash_config);
    assert_eq!(severity, SlashingSeverity::Major);
}

/// Emission schedule: 25% annual reduction compounds correctly.
#[test]
fn test_emission_schedule_annual_reduction() {
    let rewards_config = gratia_core::config::RewardsConfig::default();
    let schedule = EmissionSchedule::new(&rewards_config, 4); // 4-second blocks

    let year_0_reward = schedule.calculate_block_reward(0);
    let blocks_per_year = 365 * 24 * 3600 / 4;

    let year_1_reward = schedule.calculate_block_reward(blocks_per_year);
    let year_2_reward = schedule.calculate_block_reward(2 * blocks_per_year);

    // Year 1 should be 75% of year 0.
    assert_eq!(year_1_reward, (year_0_reward as u128 * 7500 / 10000) as Lux);

    // Year 2 should be 75% of year 1.
    assert_eq!(year_2_reward, (year_1_reward as u128 * 7500 / 10000) as Lux);

    // Rewards should never hit zero.
    let far_future = schedule.calculate_block_reward(1_000_000_000);
    assert!(far_future >= 1, "Reward should never be zero");
}

/// Reward distribution: all miners get equal base reward (flat rate).
#[test]
fn test_flat_rate_mining_rewards() {
    let rewards_config = gratia_core::config::RewardsConfig::default();
    let mut schedule = EmissionSchedule::new(&rewards_config, 4);

    let miners = vec![
        (test_node(1), GeographicTier::Standard),
        (test_node(2), GeographicTier::Standard),
        (test_node(3), GeographicTier::Standard),
    ];

    let dist = schedule.distribute_rewards(0, &miners, 15000);

    // All miners get the same base reward.
    let base_rewards: Vec<Lux> = dist.miner_rewards.iter().map(|r| r.base_reward).collect();
    assert!(
        base_rewards.windows(2).all(|w| w[0] == w[1]),
        "All miners should receive the same base reward: {:?}",
        base_rewards
    );
}

/// Geographic equity: underserved regions earn elevated rewards.
#[test]
fn test_geographic_equity_bonus() {
    let rewards_config = gratia_core::config::RewardsConfig::default();
    let mut schedule = EmissionSchedule::new(&rewards_config, 4);

    let miners = vec![
        (test_node(1), GeographicTier::Standard),
        (test_node(2), GeographicTier::Underserved),
    ];

    let dist = schedule.distribute_rewards(0, &miners, 15000);

    let standard = &dist.miner_rewards[0];
    let underserved = &dist.miner_rewards[1];

    assert_eq!(standard.geographic_bonus, 0);
    assert!(underserved.geographic_bonus > 0);
    assert!(underserved.total > standard.total);

    // Bonus should be 50% of base (15000 - 10000 = 5000 bps).
    let expected_bonus = (standard.base_reward as u128 * 5000 / 10000) as Lux;
    assert_eq!(underserved.geographic_bonus, expected_bonus);
}
