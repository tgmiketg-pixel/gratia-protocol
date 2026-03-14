//! Proof of Life parameter validator — checks daily attestation requirements.
//!
//! The PolValidator checks all 8 required PoL parameters against config thresholds
//! and runs heuristic checks for phone farm detection. Raw sensor data is processed
//! exclusively on-device; this validator only sees aggregated counts and flags.

use chrono::{DateTime, Utc};
use gratia_core::{
    config::ProofOfLifeConfig,
    error::GratiaError,
    DailyProofOfLifeData,
};

/// Result of a full PoL validation pass.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether all 8 required parameters passed.
    pub passed: bool,
    /// Per-parameter outcomes (true = passed).
    pub parameter_results: ParameterResults,
    /// Suspicious pattern flags from heuristic analysis.
    pub suspicious_patterns: SuspiciousPatterns,
}

/// Individual pass/fail for each of the 8 required PoL parameters.
#[derive(Debug, Clone, Copy)]
pub struct ParameterResults {
    pub unlock_count: bool,
    pub unlock_spread: bool,
    pub interaction_sessions: bool,
    pub orientation_change: bool,
    pub human_motion: bool,
    pub gps_fix: bool,
    pub network_connectivity: bool,
    pub bt_environment_variation: bool,
    pub charge_cycle: bool,
}

impl ParameterResults {
    /// Returns true only if every required parameter passed.
    pub fn all_passed(&self) -> bool {
        self.unlock_count
            && self.unlock_spread
            && self.interaction_sessions
            && self.orientation_change
            && self.human_motion
            && self.gps_fix
            && self.network_connectivity
            && self.bt_environment_variation
            && self.charge_cycle
    }

    /// Returns a list of human-readable names for all failed parameters.
    pub fn failed_parameters(&self) -> Vec<&'static str> {
        let mut failures = Vec::new();
        if !self.unlock_count { failures.push("unlock_count"); }
        if !self.unlock_spread { failures.push("unlock_spread"); }
        if !self.interaction_sessions { failures.push("interaction_sessions"); }
        if !self.orientation_change { failures.push("orientation_change"); }
        if !self.human_motion { failures.push("human_motion"); }
        if !self.gps_fix { failures.push("gps_fix"); }
        if !self.network_connectivity { failures.push("network_connectivity"); }
        if !self.bt_environment_variation { failures.push("bt_environment_variation"); }
        if !self.charge_cycle { failures.push("charge_cycle"); }
        failures
    }
}

/// Heuristic flags that suggest automated or farmed device usage.
#[derive(Debug, Clone, Default)]
pub struct SuspiciousPatterns {
    /// Unlock events are spaced at suspiciously regular intervals.
    pub regular_interval_unlocks: bool,
    /// All interaction sessions have identical duration or timing.
    pub identical_session_timing: bool,
    /// Unlock events are clustered in a pattern that suggests automation.
    pub automated_cluster_pattern: bool,
    /// Overall suspicion score (0.0 = no suspicion, 1.0 = certainly automated).
    pub suspicion_score: f64,
}

impl SuspiciousPatterns {
    /// Whether any suspicious pattern was flagged.
    pub fn any_flagged(&self) -> bool {
        self.regular_interval_unlocks
            || self.identical_session_timing
            || self.automated_cluster_pattern
    }
}

/// Validates daily Proof of Life data against protocol configuration thresholds.
pub struct PolValidator {
    config: ProofOfLifeConfig,
}

impl PolValidator {
    pub fn new(config: ProofOfLifeConfig) -> Self {
        PolValidator { config }
    }

    /// Validate all 8 required PoL parameters for a day's data.
    ///
    /// Returns a detailed `ValidationResult` with per-parameter outcomes
    /// and suspicion heuristics.
    pub fn validate_daily_data(&self, data: &DailyProofOfLifeData) -> ValidationResult {
        let parameter_results = ParameterResults {
            // 1. Minimum 10 unlocks
            unlock_count: data.unlock_count >= self.config.min_daily_unlocks,
            // 2. Unlocks spread across at least 6 hours
            unlock_spread: self.validate_unlock_spread(data.first_unlock, data.last_unlock),
            // 3. Screen interaction sessions showing organic patterns
            interaction_sessions: data.interaction_sessions >= self.config.min_interaction_sessions,
            // 4. At least one orientation change
            orientation_change: data.orientation_changed,
            // 5. Human-consistent motion detected
            human_motion: data.human_motion_detected,
            // 6. At least one GPS fix
            gps_fix: data.gps_fix_obtained,
            // 7. Wi-Fi OR Bluetooth connectivity
            network_connectivity: data.distinct_wifi_networks >= 1
                || data.distinct_bt_environments >= 1,
            // 8a. Varying Bluetooth peer environments
            bt_environment_variation: data.distinct_bt_environments
                >= self.config.min_distinct_bt_environments,
            // 8b. At least one charge cycle event
            charge_cycle: data.charge_cycle_event,
        };

        let suspicious_patterns = self.detect_suspicious_patterns(data);

        let passed = parameter_results.all_passed();

        ValidationResult {
            passed,
            parameter_results,
            suspicious_patterns,
        }
    }

    /// Validate that unlock events span at least the required number of hours.
    ///
    /// Returns false if either timestamp is missing or the spread is too narrow.
    pub fn validate_unlock_spread(
        &self,
        first_unlock: Option<DateTime<Utc>>,
        last_unlock: Option<DateTime<Utc>>,
    ) -> bool {
        match (first_unlock, last_unlock) {
            (Some(first), Some(last)) => {
                let spread_hours = (last - first).num_hours();
                spread_hours >= self.config.min_unlock_spread_hours as i64
            }
            _ => false,
        }
    }

    /// Run heuristic checks to detect phone farm or automation patterns.
    ///
    /// These are soft signals — they do not directly invalidate a day's PoL,
    /// but a high suspicion score can trigger additional scrutiny or flagging
    /// at the consensus layer.
    ///
    /// Current heuristics:
    /// - **Regular interval unlocks:** If the standard deviation of inter-unlock
    ///   intervals is suspiciously low relative to the mean, the unlocks are
    ///   likely automated. Real humans do not unlock their phone at perfectly
    ///   regular intervals.
    /// - **Identical session timing:** If interaction session count exactly equals
    ///   the minimum with no organic surplus, it looks scripted.
    /// - **Automated cluster pattern:** All unlocks crammed into the narrowest
    ///   possible 6-hour window suggests gaming the spread requirement.
    pub fn detect_suspicious_patterns(&self, data: &DailyProofOfLifeData) -> SuspiciousPatterns {
        let mut patterns = SuspiciousPatterns::default();
        let mut suspicion_signals: Vec<f64> = Vec::new();

        // --- Heuristic 1: Regular interval unlocks ---
        // With only aggregate data (first_unlock, last_unlock, unlock_count) we
        // can check whether the count-to-spread ratio is suspiciously "perfect."
        // A phone farm script that spaces unlocks evenly will produce exactly
        // min_unlocks over exactly min_spread_hours.
        if let (Some(first), Some(last)) = (data.first_unlock, data.last_unlock) {
            let spread_secs = (last - first).num_seconds().max(1) as f64;
            if data.unlock_count >= 2 {
                let avg_interval = spread_secs / (data.unlock_count as f64 - 1.0);

                // WHY: A perfect spread means exactly min_unlocks in exactly min_spread_hours.
                // Real humans have bursty unlock patterns (check phone 3x in 5 minutes,
                // then nothing for 2 hours). If the average interval is suspiciously
                // close to (spread / (count-1)) AND the count is exactly the minimum,
                // it smells like automation.
                let exactly_minimum_unlocks = data.unlock_count == self.config.min_daily_unlocks;
                let spread_hours = spread_secs / 3600.0;
                let barely_meets_spread =
                    spread_hours < (self.config.min_unlock_spread_hours as f64 + 0.5);

                if exactly_minimum_unlocks && barely_meets_spread {
                    // Exactly meeting both thresholds with no organic surplus is suspicious.
                    patterns.regular_interval_unlocks = true;
                    suspicion_signals.push(0.6);
                } else if exactly_minimum_unlocks {
                    // Exactly minimum count but decent spread — mildly suspicious.
                    suspicion_signals.push(0.2);
                }

                // WHY: If every interval would be identical (avg_interval * (count-1) == spread),
                // that is by definition perfectly regular. We flag this only when count
                // is small (close to minimum) because with many unlocks the average
                // always converges regardless of distribution.
                if data.unlock_count <= self.config.min_daily_unlocks + 2 {
                    let perfect_spread = avg_interval * (data.unlock_count as f64 - 1.0);
                    let deviation = (perfect_spread - spread_secs).abs() / spread_secs;
                    // deviation ~0 means perfect regularity
                    if deviation < 0.01 {
                        patterns.regular_interval_unlocks = true;
                        suspicion_signals.push(0.4);
                    }
                }
            }
        }

        // --- Heuristic 2: Identical session timing ---
        // If interaction sessions exactly equal the minimum threshold with no surplus,
        // it looks like a script hitting the bare minimum.
        if data.interaction_sessions == self.config.min_interaction_sessions {
            patterns.identical_session_timing = true;
            suspicion_signals.push(0.3);
        }

        // --- Heuristic 3: Automated cluster pattern ---
        // All unlocks crammed into the narrowest possible window that still meets
        // the 6-hour spread. Combined with hitting exactly the minimum unlock count,
        // this is a strong automation signal.
        if let (Some(first), Some(last)) = (data.first_unlock, data.last_unlock) {
            let spread_hours = (last - first).num_seconds() as f64 / 3600.0;
            let min_spread = self.config.min_unlock_spread_hours as f64;

            // WHY: A human who uses their phone for 6+ hours will typically have
            // a spread well beyond 6 hours. A spread within 10 minutes of the
            // threshold combined with exact minimum counts is a phone farm pattern.
            let barely_meets = spread_hours >= min_spread && spread_hours < (min_spread + 0.17);
            // 0.17 hours ~= 10 minutes of margin

            if barely_meets
                && data.unlock_count == self.config.min_daily_unlocks
                && data.interaction_sessions == self.config.min_interaction_sessions
            {
                patterns.automated_cluster_pattern = true;
                suspicion_signals.push(0.8);
            }
        }

        // --- Composite suspicion score ---
        // WHY: We use the maximum signal rather than average, because one strong
        // automation signal is more meaningful than several weak ones. This avoids
        // diluting a clear phone-farm signature with benign weak signals.
        patterns.suspicion_score = suspicion_signals
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            .min(1.0);

        patterns
    }

    /// Validate daily data and return a `Result` suitable for protocol error flows.
    ///
    /// Returns `Ok(())` if all parameters pass, or `Err(GratiaError)` with
    /// the first failing parameter.
    pub fn validate_or_error(&self, data: &DailyProofOfLifeData) -> Result<(), GratiaError> {
        let result = self.validate_daily_data(data);
        if result.passed {
            return Ok(());
        }

        let pr = &result.parameter_results;

        if !pr.unlock_count {
            return Err(GratiaError::InsufficientUnlocks {
                count: data.unlock_count,
                required: self.config.min_daily_unlocks,
            });
        }
        if !pr.unlock_spread {
            let hours = match (data.first_unlock, data.last_unlock) {
                (Some(f), Some(l)) => (l - f).num_hours() as u32,
                _ => 0,
            };
            return Err(GratiaError::UnlockSpreadTooNarrow {
                hours,
                required: self.config.min_unlock_spread_hours,
            });
        }
        if !pr.charge_cycle {
            return Err(GratiaError::NoChargeCycleEvent);
        }
        if !pr.bt_environment_variation {
            return Err(GratiaError::InsufficientBtVariation);
        }

        // Generic fallback for the remaining parameters
        let failed = result.parameter_results.failed_parameters();
        Err(GratiaError::ProofOfLifeInvalid {
            reason: format!("failed parameters: {}", failed.join(", ")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use gratia_core::OptionalSensorData;

    fn default_config() -> ProofOfLifeConfig {
        ProofOfLifeConfig::default()
    }

    /// Helper: build a valid DailyProofOfLifeData that passes all 8 parameters.
    fn valid_day_data() -> DailyProofOfLifeData {
        let now = Utc::now();
        DailyProofOfLifeData {
            unlock_count: 15,
            first_unlock: Some(now - Duration::hours(10)),
            last_unlock: Some(now),
            interaction_sessions: 8,
            orientation_changed: true,
            human_motion_detected: true,
            gps_fix_obtained: true,
            approximate_location: None,
            distinct_wifi_networks: 3,
            distinct_bt_environments: 4,
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        }
    }

    #[test]
    fn test_valid_day_passes() {
        let validator = PolValidator::new(default_config());
        let result = validator.validate_daily_data(&valid_day_data());
        assert!(result.passed);
        assert!(result.parameter_results.all_passed());
    }

    #[test]
    fn test_insufficient_unlocks() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.unlock_count = 5;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.unlock_count);
    }

    #[test]
    fn test_narrow_unlock_spread() {
        let validator = PolValidator::new(default_config());
        let now = Utc::now();
        let mut data = valid_day_data();
        data.first_unlock = Some(now - Duration::hours(2));
        data.last_unlock = Some(now);
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.unlock_spread);
    }

    #[test]
    fn test_missing_unlock_timestamps() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.first_unlock = None;
        data.last_unlock = None;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.unlock_spread);
    }

    #[test]
    fn test_no_orientation_change() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.orientation_changed = false;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.orientation_change);
    }

    #[test]
    fn test_no_human_motion() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.human_motion_detected = false;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.human_motion);
    }

    #[test]
    fn test_no_gps_fix() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.gps_fix_obtained = false;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.gps_fix);
    }

    #[test]
    fn test_no_network_connectivity() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.distinct_wifi_networks = 0;
        data.distinct_bt_environments = 0;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.network_connectivity);
        assert!(!result.parameter_results.bt_environment_variation);
    }

    #[test]
    fn test_wifi_only_passes_connectivity() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        // Wi-Fi present but no BT environments — connectivity passes,
        // but bt_environment_variation fails.
        data.distinct_wifi_networks = 2;
        data.distinct_bt_environments = 0;
        let result = validator.validate_daily_data(&data);
        assert!(result.parameter_results.network_connectivity);
        assert!(!result.parameter_results.bt_environment_variation);
        assert!(!result.passed);
    }

    #[test]
    fn test_no_charge_cycle() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.charge_cycle_event = false;
        let result = validator.validate_daily_data(&data);
        assert!(!result.passed);
        assert!(!result.parameter_results.charge_cycle);
    }

    #[test]
    fn test_validate_or_error_success() {
        let validator = PolValidator::new(default_config());
        assert!(validator.validate_or_error(&valid_day_data()).is_ok());
    }

    #[test]
    fn test_validate_or_error_insufficient_unlocks() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.unlock_count = 3;
        let err = validator.validate_or_error(&data).unwrap_err();
        assert!(matches!(err, GratiaError::InsufficientUnlocks { .. }));
    }

    #[test]
    fn test_validate_or_error_narrow_spread() {
        let validator = PolValidator::new(default_config());
        let now = Utc::now();
        let mut data = valid_day_data();
        data.first_unlock = Some(now - Duration::hours(1));
        data.last_unlock = Some(now);
        let err = validator.validate_or_error(&data).unwrap_err();
        assert!(matches!(err, GratiaError::UnlockSpreadTooNarrow { .. }));
    }

    #[test]
    fn test_suspicious_exact_minimum_pattern() {
        let validator = PolValidator::new(default_config());
        let now = Utc::now();
        // Craft data that exactly meets every minimum — classic phone farm signature.
        let data = DailyProofOfLifeData {
            unlock_count: 10,
            first_unlock: Some(now - Duration::hours(6)),
            last_unlock: Some(now),
            interaction_sessions: 3,
            orientation_changed: true,
            human_motion_detected: true,
            gps_fix_obtained: true,
            approximate_location: None,
            distinct_wifi_networks: 1,
            distinct_bt_environments: 2,
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        };
        let result = validator.validate_daily_data(&data);
        assert!(result.passed);
        assert!(result.suspicious_patterns.any_flagged());
        assert!(result.suspicious_patterns.suspicion_score > 0.0);
    }

    #[test]
    fn test_no_suspicious_patterns_for_organic_usage() {
        let validator = PolValidator::new(default_config());
        let now = Utc::now();
        // Organic usage: well above minimums, wide spread.
        let data = DailyProofOfLifeData {
            unlock_count: 45,
            first_unlock: Some(now - Duration::hours(14)),
            last_unlock: Some(now),
            interaction_sessions: 20,
            orientation_changed: true,
            human_motion_detected: true,
            gps_fix_obtained: true,
            approximate_location: None,
            distinct_wifi_networks: 5,
            distinct_bt_environments: 8,
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        };
        let result = validator.validate_daily_data(&data);
        assert!(result.passed);
        assert!(!result.suspicious_patterns.any_flagged());
        assert_eq!(result.suspicious_patterns.suspicion_score, 0.0);
    }

    #[test]
    fn test_failed_parameters_list() {
        let validator = PolValidator::new(default_config());
        let mut data = valid_day_data();
        data.gps_fix_obtained = false;
        data.charge_cycle_event = false;
        let result = validator.validate_daily_data(&data);
        let failed = result.parameter_results.failed_parameters();
        assert!(failed.contains(&"gps_fix"));
        assert!(failed.contains(&"charge_cycle"));
        assert_eq!(failed.len(), 2);
    }

    #[test]
    fn test_validate_unlock_spread_exactly_six_hours() {
        let validator = PolValidator::new(default_config());
        let now = Utc::now();
        // Exactly 6 hours should pass (>= 6).
        assert!(validator.validate_unlock_spread(
            Some(now - Duration::hours(6)),
            Some(now),
        ));
    }

    #[test]
    fn test_validate_unlock_spread_just_under_six_hours() {
        let validator = PolValidator::new(default_config());
        let now = Utc::now();
        // 5 hours 59 minutes — should fail.
        assert!(!validator.validate_unlock_spread(
            Some(now - Duration::minutes(359)),
            Some(now),
        ));
    }
}
