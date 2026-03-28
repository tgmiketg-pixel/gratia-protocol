//! Protocol configuration and constants.
//!
//! All tunable parameters of the Gratia protocol are defined here.
//! Parameters marked as governance-adjustable can be changed through
//! the one-phone-one-vote governance process.

use serde::{Serialize, Deserialize};

use crate::types::Lux;

/// Master protocol configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub mining: MiningConfig,
    pub proof_of_life: ProofOfLifeConfig,
    pub staking: StakingConfig,
    pub governance: GovernanceConfig,
    pub network: NetworkConfig,
    pub rewards: RewardsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mining: MiningConfig::default(),
            proof_of_life: ProofOfLifeConfig::default(),
            staking: StakingConfig::default(),
            governance: GovernanceConfig::default(),
            network: NetworkConfig::default(),
            rewards: RewardsConfig::default(),
        }
    }
}

/// Mining parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningConfig {
    /// Minimum battery percentage to activate mining.
    pub min_battery_percent: u8,
    /// Maximum CPU temperature before throttling (Celsius).
    pub max_cpu_temp_celsius: f32,
    /// How often to check power state (seconds).
    pub power_check_interval_secs: u64,
}

impl Default for MiningConfig {
    fn default() -> Self {
        MiningConfig {
            min_battery_percent: 80,
            max_cpu_temp_celsius: 40.0,
            power_check_interval_secs: 30,
        }
    }
}

/// Proof of Life parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofOfLifeConfig {
    /// Minimum unlock events per day.
    pub min_daily_unlocks: u32,
    /// Minimum hours between first and last unlock.
    pub min_unlock_spread_hours: u32,
    /// Minimum distinct screen interaction sessions.
    pub min_interaction_sessions: u32,
    /// Minimum distinct Bluetooth environments per day.
    pub min_distinct_bt_environments: u32,
    /// Number of days of valid PoL required before first mining (onboarding).
    pub onboarding_days: u32,
    /// Number of consecutive missed days before mining eligibility pauses.
    pub grace_period_days: u32,
    /// Minimum Presence Score to cross consensus threshold.
    pub consensus_threshold_score: u8,
    /// GPS sampling interval during Proof of Life (seconds).
    pub gps_sample_interval_secs: u64,
}

impl Default for ProofOfLifeConfig {
    fn default() -> Self {
        ProofOfLifeConfig {
            min_daily_unlocks: 10,
            min_unlock_spread_hours: 6,
            min_interaction_sessions: 3,
            min_distinct_bt_environments: 2,
            onboarding_days: 1,
            grace_period_days: 1,
            consensus_threshold_score: 40,
            gps_sample_interval_secs: 1800, // 30 minutes
        }
    }
}

/// Staking parameters (governance-adjustable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingConfig {
    /// Minimum stake required to participate in mining (in Lux).
    pub minimum_stake: Lux,
    /// Maximum effective stake per node (in Lux). Excess overflows to pool.
    pub per_node_cap: Lux,
    /// Cooldown period for unstaking (seconds).
    pub unstake_cooldown_secs: u64,
    /// Slash percentage for dishonest behavior (basis points, e.g., 1000 = 10%).
    pub slash_rate_bps: u32,
}

impl Default for StakingConfig {
    fn default() -> Self {
        StakingConfig {
            // WHY: Zero at genesis enables zero-delay onboarding — users install,
            // plug in, and mine immediately with no GRAT needed. PoL + energy
            // expenditure are sufficient Sybil defense for a young network.
            // Governance can raise this later when the network is large enough
            // that small-scale multi-device gaming becomes a real threat.
            minimum_stake: 0,
            per_node_cap: 1_000 * super::types::LUX_PER_GRAT, // 1,000 GRAT
            unstake_cooldown_secs: 7 * 24 * 3600,              // 7 days
            slash_rate_bps: 1000,                               // 10%
        }
    }
}

/// Governance parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceConfig {
    /// Minimum consecutive PoL days to submit a proposal.
    pub min_proposer_history_days: u64,
    /// Discussion period duration (seconds).
    pub discussion_period_secs: u64,
    /// Voting period duration (seconds).
    pub voting_period_secs: u64,
    /// Implementation delay after passage (seconds).
    pub implementation_delay_secs: u64,
    /// Passage threshold (basis points, e.g., 5100 = 51%).
    pub passage_threshold_bps: u32,
    /// Quorum requirement (basis points of eligible voters, e.g., 2000 = 20%).
    pub quorum_bps: u32,
    /// Emergency governance supermajority (basis points, e.g., 7500 = 75%).
    pub emergency_threshold_bps: u32,
    /// Emergency fix ratification deadline (seconds).
    pub emergency_ratification_secs: u64,
    /// Cost to create an on-chain poll (in Lux, burned).
    pub poll_creation_fee: Lux,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        GovernanceConfig {
            min_proposer_history_days: 90,
            discussion_period_secs: 14 * 24 * 3600,     // 14 days
            voting_period_secs: 7 * 24 * 3600,          // 7 days
            implementation_delay_secs: 30 * 24 * 3600,  // 30 days
            passage_threshold_bps: 5100,                 // 51%
            quorum_bps: 2000,                            // 20%
            emergency_threshold_bps: 7500,               // 75%
            emergency_ratification_secs: 90 * 24 * 3600, // 90 days
            poll_creation_fee: 10 * super::types::LUX_PER_GRAT, // 10 GRAT
        }
    }
}

/// Network parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Target block time (seconds).
    pub target_block_time_secs: u64,
    /// Maximum block size (bytes).
    pub max_block_size_bytes: usize,
    /// Number of validators per committee.
    pub committee_size: usize,
    /// Finality threshold (number of committee signatures required).
    pub finality_threshold: usize,
    /// Maximum state database size (bytes).
    pub max_state_db_bytes: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            target_block_time_secs: 4,           // 4 seconds (middle of 3-5 range)
            max_block_size_bytes: 262_144,       // 256 KB
            committee_size: 21,
            finality_threshold: 14,              // 14/21 = 67%
            max_state_db_bytes: 5 * 1024 * 1024 * 1024, // 5 GB
        }
    }
}

/// Reward parameters (governance-adjustable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardsConfig {
    /// Base reward per block (in Lux), shared equally among all eligible miners.
    pub block_reward: Lux,
    /// Annual emission reduction rate (basis points, e.g., 2500 = 25%).
    pub annual_reduction_bps: u32,
    /// Geographic equity multiplier for underserved regions (basis points, e.g., 15000 = 1.5x).
    pub geographic_bonus_max_bps: u32,
    /// Maximum NFC transaction amount eligible for zero fees (in Lux).
    pub zero_fee_nfc_threshold: Lux,
}

impl Default for RewardsConfig {
    fn default() -> Self {
        RewardsConfig {
            block_reward: 50 * super::types::LUX_PER_GRAT, // 50 GRAT per block (initial)
            annual_reduction_bps: 2500,                      // 25% annual reduction
            geographic_bonus_max_bps: 15000,                 // Up to 1.5x for underserved regions
            zero_fee_nfc_threshold: 10 * super::types::LUX_PER_GRAT, // 10 GRAT
        }
    }
}
