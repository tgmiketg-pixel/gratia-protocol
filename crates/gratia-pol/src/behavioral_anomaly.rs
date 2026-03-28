//! Cross-day behavioral anomaly detection for Proof of Life.
//!
//! Analyzes a rolling 30-day window of daily behavioral summaries to detect
//! replay attacks, sensor spoofing, and phone-sharing schemes. All analysis
//! happens on-device — the network only sees a "behavioral consistency score"
//! (0-100) attested via ZK proof. Raw behavioral data never leaves the phone.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// Rolling window size for behavioral analysis.
/// 30 days captures weekly patterns while keeping memory footprint small on mobile.
const MAX_WINDOW_DAYS: usize = 30;

/// Minimum days before meaningful analysis is possible.
/// 7 days captures at least one full weekly cycle of human routine.
const MIN_DAYS_FOR_ANALYSIS: usize = 7;

/// Temporal consistency: std dev of avg unlock hour below this threshold earns full points.
/// 3 hours accommodates normal weekday/weekend variation without being so loose
/// that a phone-sharing scheme slips through.
const TEMPORAL_STD_DEV_FULL: f32 = 3.0;

/// Temporal consistency: std dev above this threshold earns zero points.
/// 6+ hours of spread in average unlock hour strongly suggests multiple users.
const TEMPORAL_STD_DEV_ZERO: f32 = 6.0;

/// Behavioral variation: correlation above this threshold between any two days
/// indicates a replay attack (identical sensor data being re-submitted).
const REPLAY_CORRELATION_THRESHOLD: f64 = 0.99;

/// Behavioral variation: minimum healthy correlation between days.
/// Below 0.5 suggests wildly different usage patterns (different person).
const MIN_HEALTHY_CORRELATION: f64 = 0.5;

/// Behavioral variation: maximum healthy correlation.
/// Above 0.95 is suspiciously similar but below the replay threshold.
const MAX_HEALTHY_CORRELATION: f64 = 0.95;

/// Movement consistency: minimum coefficient of variation (std_dev / mean).
/// Below 0.2 means nearly identical daily movement — likely a stationary device
/// with spoofed GPS producing the same fake distance every day.
const MOVEMENT_CV_MIN: f64 = 0.2;

/// Movement consistency: maximum coefficient of variation.
/// Above 1.5 suggests drastically different daily movement patterns,
/// which is a signal for phone-sharing between people with different lifestyles.
const MOVEMENT_CV_MAX: f64 = 1.5;

/// Bluetooth diversity: minimum average peer count for full points.
/// A real phone carried by a real person encounters at least a few BT peers
/// per day (other phones, headphones, cars, smart devices).
const BT_DIVERSITY_MIN: f32 = 3.0;

/// Movement threshold in meters — below this across all days means static device.
/// 100 meters total in a day means the phone effectively did not move.
const STATIC_DEVICE_THRESHOLD: f64 = 100.0;

/// Interaction richness below this is considered "low interaction" even if unlocks are high.
/// A phone that is unlocked many times but has minimal touch diversity (0.15 or below)
/// suggests automated unlock scripts rather than genuine human use.
const LOW_INTERACTION_THRESHOLD: f32 = 0.15;

// ============================================================================
// Scoring weight allocations (must sum to 100)
// ============================================================================

/// Points allocated to temporal consistency (unlock hour regularity).
const TEMPORAL_WEIGHT: f32 = 30.0;
/// Points allocated to behavioral variation (not too identical, not too different).
const VARIATION_WEIGHT: f32 = 25.0;
/// Points allocated to interaction richness (genuine touch diversity).
const RICHNESS_WEIGHT: f32 = 20.0;
/// Points allocated to movement consistency (reasonable daily distance variance).
const MOVEMENT_WEIGHT: f32 = 15.0;
/// Points allocated to bluetooth diversity (encountering real-world BT peers).
const BLUETOOTH_WEIGHT: f32 = 10.0;

// ============================================================================
// Types
// ============================================================================

/// Captures the behavioral fingerprint of a single day.
///
/// All fields are summaries derived from raw sensor data — no raw data is stored.
/// This struct lives only on-device and feeds into ZK proof generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBehavioralSummary {
    /// The date this summary covers.
    pub date: DateTime<Utc>,
    /// Average hour-of-day of unlock events (0.0-24.0).
    pub avg_unlock_hour: f32,
    /// Total number of unlock events recorded.
    pub unlock_count: u32,
    /// Standard deviation of unlock hours throughout the day.
    pub unlock_hour_spread: f32,
    /// Total minutes the screen was on.
    pub total_screen_on_minutes: u32,
    /// Approximate total distance from GPS readings in meters.
    pub movement_distance_meters: f64,
    /// Number of unique Bluetooth peers seen throughout the day.
    pub bluetooth_peer_diversity: u32,
    /// Average hour when charging begins (0.0-24.0).
    pub charge_start_hour: f32,
    /// How varied the touch patterns were (0.0-1.0).
    /// Higher means more diverse interaction (scrolling, typing, tapping in
    /// different screen regions). Lower means repetitive or scripted taps.
    pub interaction_richness: f32,
}

/// Anomaly flags raised by cross-day behavioral analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnomalyFlag {
    /// Two or more days have suspiciously identical patterns (possible replay attack).
    ReplayDetected,
    /// Sudden change in behavioral profile — likely a different person using the device.
    BehavioralDiscontinuity,
    /// Fewer than 7 days recorded — insufficient data for reliable analysis.
    InsufficientData,
    /// No meaningful movement detected across multiple days (stationary farm device).
    StaticDevice,
    /// Phone unlocked frequently but minimal genuine interaction (scripted unlocks).
    LowInteraction,
}

/// Rolling 30-day behavioral analysis window.
///
/// Maintains daily summaries and computes a consistency score that gets
/// attested via ZK proof. The score is the only value that leaves the device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehavioralFingerprint {
    /// Up to 30 daily behavioral summaries, ordered chronologically.
    daily_summaries: Vec<DailyBehavioralSummary>,
    /// Composite consistency score (0-100) derived from cross-day analysis.
    consistency_score: u8,
}

impl BehavioralFingerprint {
    /// Create a new empty fingerprint with no recorded days.
    pub fn new() -> Self {
        BehavioralFingerprint {
            daily_summaries: Vec::new(),
            consistency_score: 0,
        }
    }

    /// Add a day's behavioral summary.
    /// Removes the oldest entry if the window exceeds 30 days,
    /// then recomputes the consistency score.
    pub fn add_day(&mut self, summary: DailyBehavioralSummary) {
        self.daily_summaries.push(summary);
        if self.daily_summaries.len() > MAX_WINDOW_DAYS {
            self.daily_summaries.remove(0);
        }
        self.compute_consistency_score();
    }

    /// Analyze patterns across all recorded days and update the consistency score.
    pub fn compute_consistency_score(&mut self) {
        if self.daily_summaries.is_empty() {
            self.consistency_score = 0;
            return;
        }

        // WHY: With fewer than MIN_DAYS_FOR_ANALYSIS days, statistical measures are
        // unreliable. We still compute a proportionally reduced score so early users
        // see progress, but it is naturally capped by the lack of data.
        let data_completeness = if self.daily_summaries.len() < MIN_DAYS_FOR_ANALYSIS {
            self.daily_summaries.len() as f32 / MIN_DAYS_FOR_ANALYSIS as f32
        } else {
            1.0
        };

        let temporal = self.score_temporal_consistency();
        let variation = self.score_behavioral_variation();
        let richness = self.score_interaction_richness();
        let movement = self.score_movement_consistency();
        let bluetooth = self.score_bluetooth_diversity();

        let raw_score = temporal + variation + richness + movement + bluetooth;
        let adjusted = (raw_score * data_completeness).round() as u8;

        self.consistency_score = adjusted.min(100);
    }

    /// Get the current behavioral consistency score (0-100).
    pub fn score(&self) -> u8 {
        self.consistency_score
    }

    /// Get the number of days currently recorded in the window.
    pub fn days_recorded(&self) -> usize {
        self.daily_summaries.len()
    }

    /// Detect anomalies in the behavioral data.
    /// Returns a list of flags for any detected issues.
    pub fn detect_anomalies(&self) -> Vec<AnomalyFlag> {
        let mut flags = Vec::new();

        if self.daily_summaries.len() < MIN_DAYS_FOR_ANALYSIS {
            flags.push(AnomalyFlag::InsufficientData);
        }

        if self.daily_summaries.len() >= 2 && self.has_replay_pattern() {
            flags.push(AnomalyFlag::ReplayDetected);
        }

        if self.daily_summaries.len() >= 14 && self.has_behavioral_discontinuity() {
            flags.push(AnomalyFlag::BehavioralDiscontinuity);
        }

        if self.daily_summaries.len() >= 3 && self.has_static_device() {
            flags.push(AnomalyFlag::StaticDevice);
        }

        if self.daily_summaries.len() >= 3 && self.has_low_interaction() {
            flags.push(AnomalyFlag::LowInteraction);
        }

        flags
    }

    // ========================================================================
    // Scoring sub-components
    // ========================================================================

    /// Temporal consistency (up to 30 pts).
    /// Measures how stable the user's average unlock hour is day-to-day.
    /// Humans have routines — they wake up and go to sleep at roughly the same time.
    fn score_temporal_consistency(&self) -> f32 {
        if self.daily_summaries.len() < 2 {
            // WHY: Can't measure consistency with only one day. Give benefit of the doubt.
            return TEMPORAL_WEIGHT * 0.5;
        }

        let hours: Vec<f32> = self.daily_summaries.iter().map(|d| d.avg_unlock_hour).collect();
        let std_dev = std_deviation_f32(&hours);

        if std_dev <= TEMPORAL_STD_DEV_FULL {
            TEMPORAL_WEIGHT
        } else if std_dev >= TEMPORAL_STD_DEV_ZERO {
            0.0
        } else {
            // Linear interpolation between full and zero
            let ratio = 1.0 - (std_dev - TEMPORAL_STD_DEV_FULL)
                / (TEMPORAL_STD_DEV_ZERO - TEMPORAL_STD_DEV_FULL);
            TEMPORAL_WEIGHT * ratio
        }
    }

    /// Behavioral variation (up to 25 pts).
    /// Days should be similar but NOT identical. Identical patterns = replay attack.
    /// Very different patterns = phone sharing.
    fn score_behavioral_variation(&self) -> f32 {
        if self.daily_summaries.len() < 2 {
            return VARIATION_WEIGHT * 0.5;
        }

        // Check all pairs for replay (identical patterns)
        let mut has_replay = false;
        let mut correlations = Vec::new();

        for i in 0..self.daily_summaries.len() {
            for j in (i + 1)..self.daily_summaries.len() {
                let corr = self.day_correlation(i, j);
                if corr > REPLAY_CORRELATION_THRESHOLD {
                    has_replay = true;
                }
                correlations.push(corr);
            }
        }

        if has_replay {
            // WHY: Any replay detection is a hard zero — this is a critical security signal.
            return 0.0;
        }

        // Score based on average correlation being in the healthy range
        let avg_corr = correlations.iter().sum::<f64>() / correlations.len() as f64;

        if avg_corr >= MIN_HEALTHY_CORRELATION && avg_corr <= MAX_HEALTHY_CORRELATION {
            VARIATION_WEIGHT
        } else if avg_corr < MIN_HEALTHY_CORRELATION {
            // Too different — possible phone sharing
            let ratio = avg_corr / MIN_HEALTHY_CORRELATION;
            VARIATION_WEIGHT * ratio as f32
        } else {
            // Between 0.95 and 0.99 — suspiciously similar but not proven replay
            let ratio = 1.0 - (avg_corr - MAX_HEALTHY_CORRELATION)
                / (REPLAY_CORRELATION_THRESHOLD - MAX_HEALTHY_CORRELATION);
            VARIATION_WEIGHT * ratio as f32
        }
    }

    /// Interaction richness (up to 20 pts).
    /// Average interaction_richness across all days, linearly scaled.
    fn score_interaction_richness(&self) -> f32 {
        if self.daily_summaries.is_empty() {
            return 0.0;
        }

        let avg: f32 = self.daily_summaries.iter().map(|d| d.interaction_richness).sum::<f32>()
            / self.daily_summaries.len() as f32;

        // Score = richness * weight, clamped to [0, 1]
        RICHNESS_WEIGHT * avg.clamp(0.0, 1.0)
    }

    /// Movement consistency (up to 15 pts).
    /// Daily movement distance should vary within a reasonable range.
    /// Too consistent = spoofed GPS. Too varied = different people.
    fn score_movement_consistency(&self) -> f32 {
        if self.daily_summaries.len() < 2 {
            return MOVEMENT_WEIGHT * 0.5;
        }

        let distances: Vec<f64> = self
            .daily_summaries
            .iter()
            .map(|d| d.movement_distance_meters)
            .collect();

        let mean = distances.iter().sum::<f64>() / distances.len() as f64;

        if mean < STATIC_DEVICE_THRESHOLD {
            // WHY: Near-zero movement means the device isn't being carried.
            // Even a homebound person generates some GPS variance.
            return 0.0;
        }

        let std_dev = std_deviation_f64(&distances);
        let cv = std_dev / mean; // coefficient of variation

        if cv >= MOVEMENT_CV_MIN && cv <= MOVEMENT_CV_MAX {
            MOVEMENT_WEIGHT
        } else if cv < MOVEMENT_CV_MIN {
            // Too stable — possible GPS spoofing
            let ratio = cv / MOVEMENT_CV_MIN;
            MOVEMENT_WEIGHT * ratio as f32
        } else {
            // Too variable — possible phone sharing
            let overshoot = cv - MOVEMENT_CV_MAX;
            // WHY: Gentle falloff — some people do have genuinely high variance
            // (traveling vs. staying home).
            let ratio = (1.0 - overshoot / MOVEMENT_CV_MAX).max(0.0);
            MOVEMENT_WEIGHT * ratio as f32
        }
    }

    /// Bluetooth diversity (up to 10 pts).
    /// A real phone encounters BT peers throughout the day.
    fn score_bluetooth_diversity(&self) -> f32 {
        if self.daily_summaries.is_empty() {
            return 0.0;
        }

        let avg_bt: f32 = self
            .daily_summaries
            .iter()
            .map(|d| d.bluetooth_peer_diversity as f32)
            .sum::<f32>()
            / self.daily_summaries.len() as f32;

        if avg_bt >= BT_DIVERSITY_MIN {
            BLUETOOTH_WEIGHT
        } else {
            // Linear scale from 0 to full
            BLUETOOTH_WEIGHT * (avg_bt / BT_DIVERSITY_MIN)
        }
    }

    // ========================================================================
    // Anomaly detection helpers
    // ========================================================================

    /// Compute a similarity metric between two days' behavioral vectors.
    /// Returns a value in [0.0, 1.0] where 1.0 means identical patterns.
    ///
    /// WHY: We normalize each metric to [0,1] then compute the max absolute
    /// difference across all dimensions. Using max (not mean) ensures that
    /// even a single metric with meaningful variation prevents a false replay
    /// flag. Two truly replayed days will have max_diff ~0, while normal
    /// human variation will show at least one dimension with notable movement.
    fn day_correlation(&self, i: usize, j: usize) -> f64 {
        let a = &self.daily_summaries[i];
        let b = &self.daily_summaries[j];

        let metrics_a = Self::normalize_day(a);
        let metrics_b = Self::normalize_day(b);

        let mut max_diff = 0.0_f64;
        let mut sum_sq_diff = 0.0_f64;
        for (va, vb) in metrics_a.iter().zip(metrics_b.iter()) {
            let diff = (va - vb).abs();
            if diff > max_diff {
                max_diff = diff;
            }
            sum_sq_diff += diff * diff;
        }

        // Euclidean distance normalized by sqrt(num_dimensions)
        // WHY: sqrt(8) ~ 2.83 is the max possible euclidean distance in [0,1]^8
        let euclidean = sum_sq_diff.sqrt() / (metrics_a.len() as f64).sqrt();

        // Blend of euclidean distance and max single-dimension difference.
        // WHY: Pure mean/euclidean can miss replay when values cluster in narrow
        // bands. Max-diff catches it from the other direction. Blending both
        // gives a robust similarity measure.
        let combined_diff = euclidean * 0.6 + max_diff * 0.4;

        (1.0 - combined_diff).max(0.0)
    }

    /// Normalize a day's metrics to [0, 1] range for comparison.
    fn normalize_day(day: &DailyBehavioralSummary) -> [f64; 8] {
        [
            day.avg_unlock_hour as f64 / 24.0,
            // WHY: Cap at 100 unlocks for normalization — anything above 100 is
            // already extreme and shouldn't skew the distance metric.
            (day.unlock_count as f64).min(100.0) / 100.0,
            day.unlock_hour_spread as f64 / 12.0, // Max meaningful spread is ~12 hours
            // WHY: Cap at 720 (12 hours) for normalization. More screen-on time
            // than 12 hours is unusual but shouldn't dominate the metric.
            (day.total_screen_on_minutes as f64).min(720.0) / 720.0,
            // WHY: Cap at 50km. Most people move less than 50km daily.
            (day.movement_distance_meters).min(50_000.0) / 50_000.0,
            // WHY: Cap at 50 BT peers for normalization.
            (day.bluetooth_peer_diversity as f64).min(50.0) / 50.0,
            day.charge_start_hour as f64 / 24.0,
            day.interaction_richness as f64, // Already 0-1
        ]
    }

    /// Check if any two days have suspiciously identical patterns (replay attack).
    fn has_replay_pattern(&self) -> bool {
        for i in 0..self.daily_summaries.len() {
            for j in (i + 1)..self.daily_summaries.len() {
                if self.day_correlation(i, j) > REPLAY_CORRELATION_THRESHOLD {
                    return true;
                }
            }
        }
        false
    }

    /// Check for behavioral discontinuity — the first half and second half of the
    /// window have dramatically different behavioral profiles.
    fn has_behavioral_discontinuity(&self) -> bool {
        let n = self.daily_summaries.len();
        if n < 14 {
            return false;
        }

        let mid = n / 2;

        // Compute average normalized vector for each half
        let first_half_avg = self.average_normalized_vector(0, mid);
        let second_half_avg = self.average_normalized_vector(mid, n);

        // Compute distance between the two averages
        let mut total_diff = 0.0_f64;
        for (a, b) in first_half_avg.iter().zip(second_half_avg.iter()) {
            total_diff += (a - b).abs();
        }
        let mean_diff = total_diff / first_half_avg.len() as f64;

        // WHY: A mean normalized difference of 0.3+ across all 8 dimensions is a
        // very strong signal that the usage profile changed dramatically. This
        // corresponds to roughly 7+ hours shift in unlock time, 30+ unlock count
        // difference, etc., simultaneously.
        mean_diff > 0.3
    }

    /// Compute the average normalized behavioral vector for a range of days.
    fn average_normalized_vector(&self, start: usize, end: usize) -> [f64; 8] {
        let mut avg = [0.0_f64; 8];
        let count = (end - start) as f64;

        for day in &self.daily_summaries[start..end] {
            let norm = Self::normalize_day(day);
            for (i, v) in norm.iter().enumerate() {
                avg[i] += v;
            }
        }

        for v in avg.iter_mut() {
            *v /= count;
        }
        avg
    }

    /// Check if the device shows no meaningful movement across days.
    fn has_static_device(&self) -> bool {
        if self.daily_summaries.len() < 3 {
            return false;
        }

        let static_days = self
            .daily_summaries
            .iter()
            .filter(|d| d.movement_distance_meters < STATIC_DEVICE_THRESHOLD)
            .count();

        // WHY: If more than 80% of recorded days are static, flag it.
        // Occasional zero-movement days are normal (sick day, lazy Sunday),
        // but consistently static is a farm.
        static_days as f32 / self.daily_summaries.len() as f32 > 0.8
    }

    /// Check if the device has high unlock counts but low interaction richness.
    fn has_low_interaction(&self) -> bool {
        if self.daily_summaries.len() < 3 {
            return false;
        }

        let low_interaction_days = self
            .daily_summaries
            .iter()
            .filter(|d| d.unlock_count >= 10 && d.interaction_richness < LOW_INTERACTION_THRESHOLD)
            .count();

        // WHY: If more than 70% of days have high unlocks but low richness,
        // this is a strong signal for automated/scripted unlock behavior.
        low_interaction_days as f32 / self.daily_summaries.len() as f32 > 0.7
    }
}

impl Default for BehavioralFingerprint {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Statistical helpers
// ============================================================================

/// Compute the standard deviation of a slice of f32 values.
fn std_deviation_f32(values: &[f32]) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32;
    variance.sqrt()
}

/// Compute the standard deviation of a slice of f64 values.
fn std_deviation_f64(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    variance.sqrt()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Generate a plausible human-like daily behavioral summary with slight daily variation.
    /// The `day_offset` shifts the date and adds minor jitter to simulate natural routine changes.
    fn make_realistic_day(day_offset: u32) -> DailyBehavioralSummary {
        // WHY: Using a well-distributed hash (splitmix32-style, two rounds) to
        // produce deterministic pseudo-random jitter. Each metric uses a unique
        // seed so even consecutive days differ substantially across all dimensions.
        fn hash32(input: u32) -> u32 {
            let mut x = input;
            x = x.wrapping_add(0x9e3779b9);
            x ^= x >> 16;
            x = x.wrapping_mul(0x45d9f3b);
            x ^= x >> 16;
            x = x.wrapping_mul(0x45d9f3b);
            x ^= x >> 16;
            x
        }

        fn jitter(day: u32, metric_id: u32) -> f32 {
            // Combine day and metric_id into a single seed
            let seed = day.wrapping_mul(31).wrapping_add(metric_id).wrapping_mul(17);
            let h = hash32(seed);
            // Map to [-1.0, 1.0]
            (h as f32 / u32::MAX as f32) * 2.0 - 1.0
        }

        let d = day_offset;

        DailyBehavioralSummary {
            date: Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0)
                .unwrap()
                + chrono::Duration::days(day_offset as i64),
            // Average unlock around 9-12 with +/- 1.5 hour daily variation
            avg_unlock_hour: 10.5 + jitter(d, 0) * 1.5,
            // 20-45 unlocks per day (typical smartphone user)
            unlock_count: (32.0 + jitter(d, 1) * 12.0).max(15.0) as u32,
            // Unlock spread: 3.5-6.5 hours std dev
            unlock_hour_spread: 5.0 + jitter(d, 2) * 1.5,
            // 100-240 minutes of screen time
            total_screen_on_minutes: (170.0 + jitter(d, 3) * 70.0).max(60.0) as u32,
            // 2-10 km of movement per day
            movement_distance_meters: 6000.0 + (jitter(d, 4) as f64) * 4000.0,
            // 4-18 BT peers seen
            bluetooth_peer_diversity: (11.0 + jitter(d, 5) * 7.0).max(3.0) as u32,
            // Charging around 9:30pm-1:30am
            charge_start_hour: 23.0 + jitter(d, 6) * 2.0,
            // Good interaction richness, 0.5-0.9
            interaction_richness: (0.7 + jitter(d, 7) * 0.2).clamp(0.3, 1.0),
        }
    }

    #[test]
    fn test_new_fingerprint_empty() {
        let fp = BehavioralFingerprint::new();
        assert_eq!(fp.score(), 0);
        assert_eq!(fp.days_recorded(), 0);
        assert!(fp.daily_summaries.is_empty());
    }

    #[test]
    fn test_add_day_and_score() {
        let mut fp = BehavioralFingerprint::new();
        fp.add_day(make_realistic_day(0));

        assert_eq!(fp.days_recorded(), 1);
        // With only 1 day, score should be low but non-zero
        assert!(fp.score() > 0);
    }

    #[test]
    fn test_max_30_day_window() {
        let mut fp = BehavioralFingerprint::new();

        for i in 0..35 {
            fp.add_day(make_realistic_day(i));
        }

        // Should be capped at 30
        assert_eq!(fp.days_recorded(), MAX_WINDOW_DAYS);
    }

    #[test]
    fn test_consistent_human_gets_high_score() {
        let mut fp = BehavioralFingerprint::new();

        for i in 0..30 {
            fp.add_day(make_realistic_day(i));
        }

        // A consistent human pattern over 30 days should score well
        assert!(
            fp.score() > 70,
            "Expected score > 70 for consistent human data, got {}",
            fp.score()
        );
    }

    #[test]
    fn test_replay_attack_detected() {
        let mut fp = BehavioralFingerprint::new();

        let day = make_realistic_day(0);

        // Add the same day data twice (replay attack)
        let mut day_copy = day.clone();
        day_copy.date = day.date + chrono::Duration::days(1);

        fp.add_day(day);
        fp.add_day(day_copy);

        // Add a few more normal days so we have enough data
        for i in 2..10 {
            let mut replay = make_realistic_day(0);
            replay.date = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap()
                + chrono::Duration::days(i);
            fp.add_day(replay);
        }

        let flags = fp.detect_anomalies();
        assert!(
            flags.contains(&AnomalyFlag::ReplayDetected),
            "Expected ReplayDetected flag, got {:?}",
            flags
        );

        // Score should be penalized — variation component should be 0
        // With 10 identical days the score should be notably low
        assert!(
            fp.score() < 60,
            "Expected low score for replay attack, got {}",
            fp.score()
        );
    }

    #[test]
    fn test_static_device_flagged() {
        let mut fp = BehavioralFingerprint::new();

        for i in 0..15 {
            let mut day = make_realistic_day(i);
            // Zero movement — device sitting on a shelf
            day.movement_distance_meters = 10.0;
            fp.add_day(day);
        }

        let flags = fp.detect_anomalies();
        assert!(
            flags.contains(&AnomalyFlag::StaticDevice),
            "Expected StaticDevice flag, got {:?}",
            flags
        );
    }

    #[test]
    fn test_behavioral_discontinuity() {
        let mut fp = BehavioralFingerprint::new();

        // First 15 days: one person's pattern (morning person)
        for i in 0..15 {
            let mut day = make_realistic_day(i);
            day.avg_unlock_hour = 7.0;
            day.charge_start_hour = 21.0;
            day.movement_distance_meters = 3000.0;
            day.unlock_count = 20;
            day.interaction_richness = 0.8;
            fp.add_day(day);
        }

        // Last 15 days: dramatically different pattern (night owl)
        for i in 15..30 {
            let mut day = make_realistic_day(i);
            day.avg_unlock_hour = 18.0; // +11 hours shift
            day.charge_start_hour = 6.0; // Charges in morning instead of evening
            day.movement_distance_meters = 15000.0; // 5x more movement
            day.unlock_count = 80; // 4x more unlocks
            day.interaction_richness = 0.3; // Different richness
            fp.add_day(day);
        }

        let flags = fp.detect_anomalies();
        assert!(
            flags.contains(&AnomalyFlag::BehavioralDiscontinuity),
            "Expected BehavioralDiscontinuity flag, got {:?}",
            flags
        );
    }

    #[test]
    fn test_insufficient_data_flag() {
        let mut fp = BehavioralFingerprint::new();

        for i in 0..5 {
            fp.add_day(make_realistic_day(i));
        }

        let flags = fp.detect_anomalies();
        assert!(
            flags.contains(&AnomalyFlag::InsufficientData),
            "Expected InsufficientData flag for {} days, got {:?}",
            fp.days_recorded(),
            flags
        );
    }

    #[test]
    fn test_low_interaction_flag() {
        let mut fp = BehavioralFingerprint::new();

        for i in 0..15 {
            let mut day = make_realistic_day(i);
            // High unlock count but very low interaction richness = scripted behavior
            day.unlock_count = 50;
            day.interaction_richness = 0.05;
            fp.add_day(day);
        }

        let flags = fp.detect_anomalies();
        assert!(
            flags.contains(&AnomalyFlag::LowInteraction),
            "Expected LowInteraction flag, got {:?}",
            flags
        );
    }

    #[test]
    fn test_score_improves_with_more_days() {
        let mut fp = BehavioralFingerprint::new();

        // Score after 3 days
        for i in 0..3 {
            fp.add_day(make_realistic_day(i));
        }
        let score_3_days = fp.score();

        // Score after 10 days
        for i in 3..10 {
            fp.add_day(make_realistic_day(i));
        }
        let score_10_days = fp.score();

        // Score after 20 days
        for i in 10..20 {
            fp.add_day(make_realistic_day(i));
        }
        let score_20_days = fp.score();

        // With consistent human data, score should improve as we gather more evidence
        assert!(
            score_10_days >= score_3_days,
            "Expected score to improve from {} (3 days) to {} (10 days)",
            score_3_days,
            score_10_days
        );
        assert!(
            score_20_days >= score_10_days,
            "Expected score to improve from {} (10 days) to {} (20 days)",
            score_10_days,
            score_20_days
        );
    }

    #[test]
    fn test_no_anomalies_for_clean_data() {
        let mut fp = BehavioralFingerprint::new();

        for i in 0..30 {
            fp.add_day(make_realistic_day(i));
        }

        let flags = fp.detect_anomalies();
        assert!(
            flags.is_empty(),
            "Expected no anomaly flags for clean human data, got {:?}",
            flags
        );
    }

    #[test]
    fn test_zero_bluetooth_reduces_score() {
        let mut fp_good = BehavioralFingerprint::new();
        let mut fp_no_bt = BehavioralFingerprint::new();

        for i in 0..15 {
            let good_day = make_realistic_day(i);
            let mut no_bt_day = make_realistic_day(i);
            no_bt_day.bluetooth_peer_diversity = 0;

            fp_good.add_day(good_day);
            fp_no_bt.add_day(no_bt_day);
        }

        assert!(
            fp_good.score() > fp_no_bt.score(),
            "Expected higher score with BT diversity ({}) than without ({})",
            fp_good.score(),
            fp_no_bt.score()
        );
    }
}
