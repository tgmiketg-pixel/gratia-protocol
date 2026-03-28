//! Proof of Life - Passive attestation engine for the Gratia protocol.
//!
//! This crate handles:
//! - Collecting and validating daily Proof of Life parameters
//! - Building ZK attestations from raw sensor data (on-device only)
//! - Managing onboarding and grace periods
//! - Behavioral pattern matching for wallet recovery
//!
//! PRIVACY: Raw sensor data is processed exclusively within this crate
//! on the local device. Only zero-knowledge attestations leave the phone.

pub mod collector;
pub mod validator;
pub mod behavioral;
pub mod behavioral_anomaly;
pub mod clustering;
pub mod trust;
pub mod tee;
pub mod scoring;

use gratia_core::{
    DailyProofOfLifeData, MiningState, PowerState, GeoLocation,
    config::Config,
};
use chrono::{NaiveDate, Utc};
use std::collections::VecDeque;

/// Manages Proof of Life state for a single node.
pub struct ProofOfLifeManager {
    config: Config,
    current_day_data: DailyProofOfLifeData,
    valid_days: VecDeque<NaiveDate>,
    consecutive_valid_days: u64,
    consecutive_missed_days: u32,
    onboarding_complete: bool,
    mining_eligible: bool,
}

impl ProofOfLifeManager {
    pub fn new(config: Config) -> Self {
        ProofOfLifeManager {
            config,
            current_day_data: DailyProofOfLifeData {
                unlock_count: 0,
                first_unlock: None,
                last_unlock: None,
                interaction_sessions: 0,
                orientation_changed: false,
                human_motion_detected: false,
                gps_fix_obtained: false,
                approximate_location: None,
                distinct_wifi_networks: 0,
                distinct_bt_environments: 0,
                charge_cycle_event: false,
                optional_sensors: Default::default(),
            },
            valid_days: VecDeque::new(),
            consecutive_valid_days: 0,
            consecutive_missed_days: 0,
            onboarding_complete: false,
            mining_eligible: false,
        }
    }

    /// Record a phone unlock event.
    pub fn record_unlock(&mut self) {
        let now = Utc::now();
        self.current_day_data.unlock_count += 1;
        if self.current_day_data.first_unlock.is_none() {
            self.current_day_data.first_unlock = Some(now);
        }
        self.current_day_data.last_unlock = Some(now);
    }

    /// Record a screen interaction session.
    pub fn record_interaction_session(&mut self) {
        self.current_day_data.interaction_sessions += 1;
    }

    /// Record an orientation change.
    pub fn record_orientation_change(&mut self) {
        self.current_day_data.orientation_changed = true;
    }

    /// Record human-consistent motion detection.
    pub fn record_human_motion(&mut self) {
        self.current_day_data.human_motion_detected = true;
    }

    /// Record a GPS fix.
    pub fn record_gps_fix(&mut self, lat: f32, lon: f32) {
        self.current_day_data.gps_fix_obtained = true;
        self.current_day_data.approximate_location = Some(GeoLocation { lat, lon });
    }

    /// Record a distinct Wi-Fi network seen.
    pub fn record_wifi_network(&mut self) {
        self.current_day_data.distinct_wifi_networks += 1;
    }

    /// Record a Bluetooth environment change.
    pub fn record_bt_environment_change(&mut self) {
        self.current_day_data.distinct_bt_environments += 1;
    }

    /// Record a charge cycle event (plug/unplug).
    pub fn record_charge_event(&mut self) {
        self.current_day_data.charge_cycle_event = true;
    }

    /// Finalize the day's Proof of Life. Returns whether the day was valid.
    pub fn finalize_day(&mut self) -> bool {
        let today = Utc::now().date_naive();
        let is_valid = self.current_day_data.is_valid(&self.config.proof_of_life);

        if is_valid {
            self.valid_days.push_back(today);
            self.consecutive_valid_days += 1;
            self.consecutive_missed_days = 0;

            if !self.onboarding_complete {
                self.onboarding_complete = true;
            }

            self.mining_eligible = true;

            while self.valid_days.len() > 365 {
                self.valid_days.pop_front();
            }
        } else {
            self.consecutive_missed_days += 1;

            if self.consecutive_missed_days >= self.config.proof_of_life.grace_period_days + 1 {
                self.mining_eligible = false;
                self.consecutive_valid_days = 0;
            }
        }

        // Reset for next day
        self.current_day_data = DailyProofOfLifeData {
            unlock_count: 0,
            first_unlock: None,
            last_unlock: None,
            interaction_sessions: 0,
            orientation_changed: false,
            human_motion_detected: false,
            gps_fix_obtained: false,
            approximate_location: None,
            distinct_wifi_networks: 0,
            distinct_bt_environments: 0,
            charge_cycle_event: false,
            optional_sensors: Default::default(),
        };

        is_valid
    }

    /// Check if this node is currently eligible for mining.
    pub fn is_mining_eligible(&self) -> bool {
        self.onboarding_complete && self.mining_eligible
    }

    /// Check if onboarding is complete.
    pub fn is_onboarded(&self) -> bool {
        self.onboarding_complete
    }

    /// Get total valid PoL days.
    pub fn participation_days(&self) -> u64 {
        self.valid_days.len() as u64
    }

    /// Get consecutive valid days.
    pub fn consecutive_days(&self) -> u64 {
        self.consecutive_valid_days
    }

    /// Save PoL state to a file for persistence across restarts.
    /// WHY: The consecutive day streak and onboarding status must survive
    /// app restarts. Without this, the trust tier resets to Unverified
    /// every time the app is killed and restarted.
    pub fn save_state(&self, data_dir: &str) {
        let path = format!("{}/pol_state.bin", data_dir);
        // Format: 8 bytes consecutive_days + 8 bytes total_days + 1 byte onboarded
        let mut data = Vec::with_capacity(17);
        data.extend_from_slice(&self.consecutive_valid_days.to_le_bytes());
        data.extend_from_slice(&(self.valid_days.len() as u64).to_le_bytes());
        data.push(if self.onboarding_complete { 1 } else { 0 });
        let _ = std::fs::write(&path, &data);
    }

    /// Load PoL state from a file.
    pub fn load_state(&mut self, data_dir: &str) {
        let path = format!("{}/pol_state.bin", data_dir);
        if let Ok(data) = std::fs::read(&path) {
            if data.len() >= 17 {
                self.consecutive_valid_days = u64::from_le_bytes(
                    data[0..8].try_into().unwrap_or([0; 8]),
                );
                let total_days = u64::from_le_bytes(
                    data[8..16].try_into().unwrap_or([0; 8]),
                );
                self.onboarding_complete = data[16] != 0;
                self.mining_eligible = self.onboarding_complete;

                // WHY: Reconstruct valid_days with placeholder dates.
                // The exact dates don't matter for the streak counter —
                // what matters is the count for trust tier calculation.
                let today = Utc::now().date_naive();
                self.valid_days.clear();
                for i in 0..total_days.min(365) {
                    if let Some(d) = today.checked_sub_signed(chrono::Duration::days(i as i64)) {
                        self.valid_days.push_back(d);
                    }
                }

                tracing::info!(
                    consecutive = self.consecutive_valid_days,
                    total = total_days,
                    onboarded = self.onboarding_complete,
                    "PoL state restored from persistence"
                );
            }
        }
    }

    /// Check current day's validity in real-time.
    pub fn current_day_valid(&self) -> bool {
        self.current_day_data.is_valid(&self.config.proof_of_life)
    }

    /// Determine mining state based on current conditions.
    pub fn determine_mining_state(&self, power: &PowerState, has_minimum_stake: bool) -> MiningState {
        if !self.is_mining_eligible() {
            return MiningState::ProofOfLife;
        }
        if !power.is_plugged_in {
            return MiningState::ProofOfLife;
        }
        if power.is_throttled {
            return MiningState::Throttled;
        }
        if power.battery_percent < self.config.mining.min_battery_percent {
            return MiningState::BatteryLow;
        }
        if !has_minimum_stake {
            return MiningState::PendingActivation;
        }
        MiningState::Mining
    }
}
