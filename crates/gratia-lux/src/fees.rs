//! Dynamic posting fee calculator for Lux.
//!
//! Posts cost more when blocks are full, less when they're empty.
//! The market self-regulates: casual posts wait for cheap blocks,
//! important posts pay more, and financial transactions are never
//! crowded out by social media.
//!
//! ## Formula
//!
//! ```text
//! fee(u) = max(1, ⌊ e^(K × max(0, u_ema - T)²) ⌋)
//! ```
//!
//! Where:
//! - `u_ema` = exponential moving average of block utilization (0.0 to 1.0)
//! - `T` = 0.5 (target utilization — we want blocks half-full)
//! - `K` = 45.5 (calibration constant)
//! - Result is in Lux (smallest GRAT unit)
//!
//! ## Smoothing
//!
//! ```text
//! u_ema(n) = α × u(n) + (1 - α) × u_ema(n-1)
//! ```
//!
//! Where α = 0.125, weighting the last ~8 blocks (~32 seconds).
//! This prevents fee spikes from a single anomalous block and makes
//! fee manipulation economically impractical.
//!
//! ## Why quadratic exponent
//!
//! A linear exponent (like EIP-1559) rises too fast in the warning zone
//! and not fast enough near capacity. The quadratic `(u - T)²` creates a
//! natural hockey stick: flat in the safe zone, then an aggressive wall
//! near congestion. This matches human expectations — posting feels free
//! until the network actually needs protection.
//!
//! ## Fee schedule
//!
//! | Utilization | Fee (Lux) | Fee (GRAT) | Effect |
//! |:-:|:-:|:-:|:--|
//! | 0-50% | 1 | 0.000001 | Free zone |
//! | 60% | 2 | 0.000002 | Unnoticeable |
//! | 70% | 6 | 0.000006 | Still trivial |
//! | 80% | 60 | 0.00006 | Casual spam drops |
//! | 85% | 263 | 0.000263 | Users think twice |
//! | 90% | 1,453 | 0.001453 | Only intentional posts |
//! | 95% | 10,000 | 0.01 | Serious deterrent |
//! | 100% | 87,000 | 0.087 | Emergency relief |
//!
//! All fees are burned — deflationary. More usage = more burn = more scarce.

use serde::{Deserialize, Serialize};
use tracing::debug;

/// Target block utilization. Below this, posting is at minimum cost.
///
/// WHY: 50% target leaves ample room for financial transactions and smart
/// contracts. Lux posts should never consume more than half the block space
/// under normal conditions.
const TARGET_UTILIZATION: f64 = 0.5;

/// Steepness constant for the exponential fee curve.
///
/// WHY: Calibrated so that fee ≈ 10,000 Lux at 95% utilization. This makes
/// posting prohibitively expensive only during genuine congestion, while
/// keeping fees trivial during normal operation. The value 45.5 was derived
/// from: k = ln(10000) / (0.95 - 0.5)² = 9.21 / 0.2025 ≈ 45.5
const FEE_STEEPNESS: f64 = 45.5;

/// EMA smoothing factor (α).
///
/// WHY: 0.125 means each new block contributes 12.5% to the average,
/// effectively weighting the last ~8 blocks (~32 seconds at 4s block time).
/// This prevents a single spam-filled block from spiking fees while still
/// responding quickly to sustained congestion.
const EMA_ALPHA: f64 = 0.125;

/// Minimum post fee in Lux. Never goes below this.
///
/// WHY: Even at 0% utilization, posting costs 1 Lux. This prevents
/// zero-cost spam and ensures every post has a tiny deflationary contribution.
const MIN_FEE_LUX: u64 = 1;

/// Flat fee for likes and reposts (always 1 Lux regardless of utilization).
///
/// WHY: Engagement actions are tiny (~120 bytes) and can't meaningfully
/// congest the network. Keeping them cheap encourages interaction.
pub const LIKE_FEE_LUX: u64 = 1;

/// Flat fee for reposts (always 1 Lux).
pub const REPOST_FEE_LUX: u64 = 1;

/// Block size in bytes (must match consensus config).
const BLOCK_SIZE_BYTES: u64 = 262_144; // 256 KB

/// Tracks block utilization and computes dynamic posting fees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeCalculator {
    /// Exponential moving average of block utilization (0.0 to 1.0).
    ema_utilization: f64,

    /// The current computed post fee in Lux.
    current_fee_lux: u64,

    /// Total Lux burned from Lux posting fees (lifetime).
    total_burned_lux: u64,

    /// Number of blocks processed.
    blocks_processed: u64,
}

impl FeeCalculator {
    pub fn new() -> Self {
        Self {
            ema_utilization: 0.0,
            current_fee_lux: MIN_FEE_LUX,
            total_burned_lux: 0,
            blocks_processed: 0,
        }
    }

    /// Update the fee calculator with a new block's utilization.
    ///
    /// Call this once per finalized block, passing the block's total
    /// byte size (all transactions + Lux posts + overhead).
    pub fn on_block_finalized(&mut self, block_bytes_used: u64) {
        let utilization = (block_bytes_used as f64) / (BLOCK_SIZE_BYTES as f64);
        let utilization = utilization.clamp(0.0, 1.0);

        // Update EMA
        self.ema_utilization = EMA_ALPHA * utilization + (1.0 - EMA_ALPHA) * self.ema_utilization;
        self.blocks_processed += 1;

        // Compute new fee
        self.current_fee_lux = compute_fee(self.ema_utilization);

        debug!(
            utilization = format!("{:.1}%", utilization * 100.0),
            ema = format!("{:.1}%", self.ema_utilization * 100.0),
            fee = self.current_fee_lux,
            "Lux fee updated"
        );
    }

    /// Get the current posting fee in Lux.
    pub fn post_fee(&self) -> u64 {
        self.current_fee_lux
    }

    /// Get the current EMA utilization (0.0 to 1.0).
    pub fn ema_utilization(&self) -> f64 {
        self.ema_utilization
    }

    /// Record a fee burn from a posted Lux message.
    pub fn record_burn(&mut self, lux_amount: u64) {
        self.total_burned_lux += lux_amount;
    }

    /// Total Lux burned from Lux fees over the network's lifetime.
    pub fn total_burned(&self) -> u64 {
        self.total_burned_lux
    }

    /// Number of blocks processed by this calculator.
    pub fn blocks_processed(&self) -> u64 {
        self.blocks_processed
    }
}

/// Pure function: compute the posting fee for a given utilization.
///
/// ```text
/// fee(u) = max(1, ⌊ e^(45.5 × max(0, u - 0.5)²) ⌋)
/// ```
fn compute_fee(ema_utilization: f64) -> u64 {
    let excess = (ema_utilization - TARGET_UTILIZATION).max(0.0);
    let exponent = FEE_STEEPNESS * excess * excess;

    // e^exponent, clamped to prevent overflow
    // WHY: At u=1.0, exponent = 45.5 × 0.25 = 11.375, e^11.375 ≈ 87,000.
    // We cap at e^20 ≈ 485 million Lux (485 GRAT) as an absolute ceiling.
    let exponent = exponent.min(20.0);
    let fee = exponent.exp();

    (fee as u64).max(MIN_FEE_LUX)
}

/// Estimate the fee for a specific utilization (for UI display).
///
/// This is a pure function that doesn't require a FeeCalculator instance.
/// Useful for showing users "if the network were X% full, posting would cost Y."
pub fn estimate_fee_at_utilization(utilization: f64) -> u64 {
    compute_fee(utilization.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_at_zero_utilization() {
        assert_eq!(compute_fee(0.0), 1);
    }

    #[test]
    fn test_fee_at_target() {
        assert_eq!(compute_fee(0.5), 1);
    }

    #[test]
    fn test_fee_below_target() {
        // Below 50% should always be 1 Lux
        assert_eq!(compute_fee(0.1), 1);
        assert_eq!(compute_fee(0.3), 1);
        assert_eq!(compute_fee(0.49), 1);
    }

    #[test]
    fn test_fee_increases_with_utilization() {
        let fee_60 = compute_fee(0.6);
        let fee_70 = compute_fee(0.7);
        let fee_80 = compute_fee(0.8);
        let fee_90 = compute_fee(0.9);
        let fee_95 = compute_fee(0.95);

        assert!(fee_60 < fee_70);
        assert!(fee_70 < fee_80);
        assert!(fee_80 < fee_90);
        assert!(fee_90 < fee_95);
    }

    #[test]
    fn test_fee_at_known_points() {
        // Verify calibration against the published fee schedule
        assert!(compute_fee(0.6) <= 5);         // ~2 Lux
        assert!(compute_fee(0.7) <= 15);         // ~6 Lux
        assert!(compute_fee(0.8) >= 30);         // ~60 Lux
        assert!(compute_fee(0.8) <= 100);
        assert!(compute_fee(0.9) >= 500);        // ~1,453 Lux
        assert!(compute_fee(0.9) <= 3000);
        assert!(compute_fee(0.95) >= 5000);      // ~10,000 Lux
        assert!(compute_fee(0.95) <= 20000);
    }

    #[test]
    fn test_fee_capped_at_max() {
        // Even at 100%, fee shouldn't overflow
        let fee = compute_fee(1.0);
        assert!(fee < 500_000_000); // Under 500 GRAT absolute max
    }

    #[test]
    fn test_ema_smoothing() {
        let mut calc = FeeCalculator::new();

        // Feed 10 blocks at 30% utilization — fee should stay at 1
        for _ in 0..10 {
            calc.on_block_finalized(78_643); // 30% of 262,144
        }
        assert_eq!(calc.post_fee(), 1);

        // One spike to 100% — fee should barely move due to EMA
        calc.on_block_finalized(262_144);
        assert!(calc.post_fee() < 10, "Single spike shouldn't cause high fee");

        // Sustained 90% for 20 blocks — fee should rise significantly
        for _ in 0..20 {
            calc.on_block_finalized(235_929); // 90% of 262,144
        }
        assert!(calc.post_fee() > 100, "Sustained congestion should raise fee");
    }

    #[test]
    fn test_burn_tracking() {
        let mut calc = FeeCalculator::new();
        assert_eq!(calc.total_burned(), 0);

        calc.record_burn(100);
        calc.record_burn(250);
        assert_eq!(calc.total_burned(), 350);
    }

    #[test]
    fn test_engagement_fees_are_flat() {
        // Likes and reposts always cost 1 Lux regardless of utilization
        assert_eq!(LIKE_FEE_LUX, 1);
        assert_eq!(REPOST_FEE_LUX, 1);
    }
}
