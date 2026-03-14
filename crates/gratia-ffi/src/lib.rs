//! gratia-ffi — UniFFI bridge for the Gratia protocol.
//!
//! This crate is the **single entry point** for the mobile apps (Android/iOS).
//! It wraps internal Rust crates and exposes a simplified, mobile-friendly API.
//! UniFFI auto-generates Kotlin and Swift bindings from the exported types and
//! functions defined here.
//!
//! ## Architecture
//!
//! ```text
//! Kotlin/Swift UI ──> UniFFI bindings ──> GratiaNode (this crate)
//!                                              │
//!                         ┌────────────────────┼────────────────────┐
//!                         ▼                    ▼                    ▼
//!                   gratia-wallet        gratia-pol          gratia-staking
//!                         │                    │                    │
//!                         └────────────────────┴────────────────────┘
//!                                              │
//!                                        gratia-core
//! ```
//!
//! All types crossing the FFI boundary are simple structs/enums with only
//! primitive fields, strings, and Vec<T>. No generics, no trait objects, no
//! lifetimes. Internal errors are mapped to a flat `FfiError` enum.

pub mod convert;

use std::sync::Mutex;

use chrono::Utc;
use tracing::{error, info, warn};

use gratia_core::config::Config;
use gratia_core::types::{MiningState, PowerState};
use gratia_pol::collector::SensorEventBuffer;
use gratia_pol::ProofOfLifeManager;
use gratia_staking::StakingManager;
use gratia_wallet::WalletManager;

use crate::convert::{address_from_hex, address_to_hex, mining_state_to_string};

// Re-export uniffi scaffolding. This macro generates the C-level FFI symbols
// that UniFFI's generated Kotlin/Swift code calls into.
uniffi::setup_scaffolding!();

// ============================================================================
// FFI Error Type
// ============================================================================

/// User-friendly error type exposed across the FFI boundary.
///
/// Variants are kept simple and descriptive — mobile UI code switches on the
/// variant name to decide what to show the user (e.g., a "battery too low"
/// toast vs. a "wallet locked" dialog).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("Wallet not initialized: call create_wallet() first")]
    WalletNotInitialized,

    #[error("Wallet already exists on this device")]
    WalletAlreadyExists,

    #[error("Invalid address format: {reason}")]
    InvalidAddress { reason: String },

    #[error("Insufficient balance: have {available_lux} Lux, need {required_lux} Lux")]
    InsufficientBalance {
        available_lux: u64,
        required_lux: u64,
    },

    #[error("Mining conditions not met: {reason}")]
    MiningNotAvailable { reason: String },

    #[error("Staking error: {reason}")]
    StakingError { reason: String },

    #[error("Proof of Life error: {reason}")]
    ProofOfLifeError { reason: String },

    #[error("Wallet is frozen due to an active recovery claim")]
    WalletFrozen,

    #[error("Internal error: {reason}")]
    InternalError { reason: String },
}

/// Map any `GratiaError` from the core crates into an `FfiError`.
///
/// WHY: We collapse the detailed internal error variants into a smaller set of
/// FFI-friendly variants. Mobile code doesn't need the full granularity — it
/// needs enough to show the right UI.
impl From<gratia_core::error::GratiaError> for FfiError {
    fn from(e: gratia_core::error::GratiaError) -> Self {
        use gratia_core::error::GratiaError;
        match e {
            GratiaError::InsufficientBalance {
                available,
                required,
            } => FfiError::InsufficientBalance {
                available_lux: available,
                required_lux: required,
            },
            GratiaError::RecoveryClaimPending => FfiError::WalletFrozen,
            GratiaError::WalletLocked => FfiError::WalletNotInitialized,
            GratiaError::NotPluggedIn
            | GratiaError::BatteryTooLow { .. }
            | GratiaError::ThermalThrottle { .. }
            | GratiaError::MiningConditionsNotMet { .. } => FfiError::MiningNotAvailable {
                reason: e.to_string(),
            },
            GratiaError::InsufficientStake { .. } | GratiaError::UnstakeCooldownActive { .. } => {
                FfiError::StakingError {
                    reason: e.to_string(),
                }
            }
            GratiaError::ProofOfLifeInvalid { .. }
            | GratiaError::InsufficientUnlocks { .. }
            | GratiaError::UnlockSpreadTooNarrow { .. }
            | GratiaError::NoChargeCycleEvent
            | GratiaError::InsufficientBtVariation
            | GratiaError::OnboardingIncomplete { .. } => FfiError::ProofOfLifeError {
                reason: e.to_string(),
            },
            other => FfiError::InternalError {
                reason: other.to_string(),
            },
        }
    }
}

// ============================================================================
// FFI Data Types
// ============================================================================

/// Wallet information returned to the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWalletInfo {
    /// Wallet address as "grat:<hex>" string.
    pub address: String,
    /// Balance in Lux (1 GRAT = 1,000,000 Lux).
    pub balance_lux: u64,
    /// Current mining state as a human-readable string.
    pub mining_state: String,
}

/// A single transaction record for display in the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTransactionInfo {
    /// Transaction hash as hex string.
    pub hash_hex: String,
    /// "sent" or "received".
    pub direction: String,
    /// Counterparty address (None for stake/unstake operations).
    pub counterparty: Option<String>,
    /// Amount in Lux.
    pub amount_lux: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
    /// "pending", "confirmed", or "failed".
    pub status: String,
}

/// Current mining status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMiningStatus {
    /// Mining state string: "proof_of_life", "pending_activation", "mining",
    /// "throttled", or "battery_low".
    pub state: String,
    /// Current battery percentage (0-100).
    pub battery_percent: u8,
    /// Whether the phone is connected to power.
    pub is_plugged_in: bool,
    /// Whether today's Proof of Life is valid.
    pub current_day_pol_valid: bool,
    /// Composite Presence Score (40-100, or 0 if not yet calculated).
    pub presence_score: u8,
}

/// Proof of Life status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProofOfLifeStatus {
    /// Whether today's PoL requirements have been met.
    pub is_valid_today: bool,
    /// Number of consecutive valid PoL days.
    pub consecutive_days: u64,
    /// Whether the one-day onboarding period is complete.
    pub is_onboarded: bool,
    /// List of parameter names that have been satisfied today.
    pub parameters_met: Vec<String>,
}

/// Staking information for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStakeInfo {
    /// Effective stake counting toward consensus (capped at per-node cap), in Lux.
    pub node_stake_lux: u64,
    /// Amount overflowed to the Network Security Pool, in Lux.
    pub overflow_amount_lux: u64,
    /// Total committed stake (effective + overflow), in Lux.
    pub total_committed_lux: u64,
    /// Unix timestamp in milliseconds when the stake was placed.
    pub staked_at_millis: i64,
    /// Whether this node meets the minimum stake requirement.
    pub meets_minimum: bool,
}

/// Sensor events pushed from the native platform layer (Android/iOS) into
/// the Rust PoL engine.
///
/// WHY: This enum mirrors `gratia_pol::collector::SensorEvent` but strips
/// out the `DateTime<Utc>` timestamp field (which is not FFI-safe). The
/// timestamp is set to `Utc::now()` on the Rust side when the event arrives.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSensorEvent {
    /// Phone was unlocked by the user.
    Unlock,
    /// A screen interaction session was recorded.
    Interaction {
        /// Duration of the session in seconds.
        duration_secs: u32,
    },
    /// Phone orientation changed (picked up, rotated, set down).
    OrientationChange,
    /// Accelerometer detected human-consistent motion.
    Motion,
    /// A GPS fix was obtained.
    GpsUpdate {
        lat: f32,
        lon: f32,
    },
    /// Wi-Fi scan completed with visible BSSIDs (as opaque hashes).
    WifiScan {
        bssid_hashes: Vec<u64>,
    },
    /// Bluetooth scan completed with nearby peers (as opaque hashes).
    BluetoothScan {
        peer_hashes: Vec<u64>,
    },
    /// Charge state changed (plugged in or unplugged).
    ChargeEvent {
        is_charging: bool,
    },
}

// ============================================================================
// GratiaNode — The main FFI entry point
// ============================================================================

/// The main API object exposed to mobile apps via UniFFI.
///
/// A single `GratiaNode` instance is created at app launch and held for the
/// lifetime of the app. It owns all subsystem managers (wallet, PoL, staking)
/// and coordinates their interactions.
///
/// Thread safety: all internal state is behind a `Mutex` so that concurrent
/// calls from the native UI thread and background services are safe.
#[derive(uniffi::Object)]
pub struct GratiaNode {
    /// Data directory for persistent storage (e.g., app-internal storage path).
    data_dir: String,
    /// Inner state protected by a mutex for thread safety across FFI calls.
    inner: Mutex<GratiaNodeInner>,
}

/// Mutable inner state of the GratiaNode.
struct GratiaNodeInner {
    wallet: WalletManager,
    pol: ProofOfLifeManager,
    sensor_buffer: SensorEventBuffer,
    staking: StakingManager,
    /// Cached power state from the last `update_power_state` call.
    power_state: PowerState,
    /// Cached mining state derived from current conditions.
    mining_state: MiningState,
    /// Composite Presence Score (placeholder — will be calculated from sensor flags).
    presence_score: u8,
}

#[uniffi::export]
impl GratiaNode {
    /// Create a new GratiaNode instance.
    ///
    /// `data_dir` is the path to the app's private data directory where
    /// persistent state (wallet keys, PoL history, etc.) will be stored.
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Self {
        let config = Config::default();

        info!("initializing GratiaNode with data_dir: {}", data_dir);

        let inner = GratiaNodeInner {
            wallet: WalletManager::new_software(),
            pol: ProofOfLifeManager::new(config.clone()),
            sensor_buffer: SensorEventBuffer::new(),
            staking: StakingManager::new(config.staking),
            power_state: PowerState {
                is_plugged_in: false,
                battery_percent: 0,
                // WHY: Default CPU temp of 25C is a safe baseline. The native layer
                // will update this via update_power_state() with real readings.
                cpu_temp_celsius: 25.0,
                is_throttled: false,
            },
            mining_state: MiningState::ProofOfLife,
            presence_score: 0,
        };

        GratiaNode {
            data_dir,
            inner: Mutex::new(inner),
        }
    }

    // ========================================================================
    // Wallet methods
    // ========================================================================

    /// Generate a new wallet keypair. Returns the wallet address string.
    ///
    /// Can only be called once per device. Returns `WalletAlreadyExists` if
    /// a wallet already exists.
    pub fn create_wallet(&self) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.create_wallet().map_err(|e| {
            if e.to_string().contains("already exists") {
                FfiError::WalletAlreadyExists
            } else {
                FfiError::from(e)
            }
        })?;
        Ok(address_to_hex(&address))
    }

    /// Get current wallet information (address, balance, mining state).
    pub fn get_wallet_info(&self) -> Result<FfiWalletInfo, FfiError> {
        let inner = self.lock_inner()?;
        let address = inner
            .wallet
            .address()
            .map_err(|_| FfiError::WalletNotInitialized)?;

        Ok(FfiWalletInfo {
            address: address_to_hex(&address),
            balance_lux: inner.wallet.balance(),
            mining_state: mining_state_to_string(&inner.mining_state),
        })
    }

    /// Send a GRAT transfer to another address.
    ///
    /// `to` is the recipient address as a hex string (with or without "grat:" prefix).
    /// `amount` is the transfer amount in Lux.
    ///
    /// Returns the transaction hash as a hex string.
    pub fn send_transfer(&self, to: String, amount: u64) -> Result<String, FfiError> {
        let recipient = address_from_hex(&to).map_err(|reason| FfiError::InvalidAddress { reason })?;

        let mut inner = self.lock_inner()?;

        // WHY: Use a fixed fee of 1000 Lux (~0.001 GRAT) as a placeholder.
        // In production, the fee will be dynamically calculated based on
        // network congestion and transaction size.
        let fee: u64 = 1000; // Placeholder fee — ~0.001 GRAT

        let tx = inner.wallet.send_transfer(recipient, amount, fee)?;
        let hash_hex = hex::encode(tx.hash.0);
        info!("FFI: transfer sent, hash={}", hash_hex);
        Ok(hash_hex)
    }

    /// Get the transaction history for this wallet.
    pub fn get_transaction_history(&self) -> Result<Vec<FfiTransactionInfo>, FfiError> {
        let inner = self.lock_inner()?;
        let history: Vec<FfiTransactionInfo> = inner
            .wallet
            .history()
            .iter()
            .map(FfiTransactionInfo::from)
            .collect();
        Ok(history)
    }

    // ========================================================================
    // Mining methods
    // ========================================================================

    /// Get the current mining status.
    pub fn get_mining_status(&self) -> Result<FfiMiningStatus, FfiError> {
        let inner = self.lock_inner()?;
        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: inner.pol.current_day_valid(),
            presence_score: inner.presence_score,
        })
    }

    /// Update the phone's power state from the native layer.
    ///
    /// Called by the Android/iOS battery manager whenever the charging state
    /// or battery level changes. This triggers a re-evaluation of whether
    /// mining conditions are met.
    pub fn update_power_state(
        &self,
        is_plugged_in: bool,
        battery_percent: u8,
    ) -> Result<FfiMiningStatus, FfiError> {
        let mut inner = self.lock_inner()?;

        inner.power_state.is_plugged_in = is_plugged_in;
        inner.power_state.battery_percent = battery_percent;

        // Recalculate mining state based on new power conditions.
        let has_min_stake = inner.staking.meets_minimum_stake(
            // WHY: We need the NodeId to check stake, but the wallet may not
            // be initialized yet. Use a zeroed NodeId as a safe fallback —
            // meets_minimum_stake will return false, which is correct behavior
            // before the wallet is created.
            &self.get_node_id_or_default(&inner),
        );
        inner.mining_state = inner.pol.determine_mining_state(&inner.power_state, has_min_stake);

        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: inner.pol.current_day_valid(),
            presence_score: inner.presence_score,
        })
    }

    /// Request to start mining.
    ///
    /// Returns the current mining status. Mining will only activate if all
    /// conditions are met (plugged in, battery >= 80%, valid PoL, minimum stake).
    pub fn start_mining(&self) -> Result<FfiMiningStatus, FfiError> {
        let mut inner = self.lock_inner()?;

        if !inner.power_state.is_plugged_in {
            return Err(FfiError::MiningNotAvailable {
                reason: "phone must be plugged in to mine".into(),
            });
        }
        if inner.power_state.battery_percent < 80 {
            return Err(FfiError::MiningNotAvailable {
                reason: format!(
                    "battery at {}%, must be at least 80%",
                    inner.power_state.battery_percent
                ),
            });
        }
        if !inner.pol.is_mining_eligible() {
            return Err(FfiError::MiningNotAvailable {
                reason: "Proof of Life not yet valid for today".into(),
            });
        }

        let node_id = self.get_node_id_or_default(&inner);
        let has_min_stake = inner.staking.meets_minimum_stake(&node_id);
        if !has_min_stake {
            return Err(FfiError::MiningNotAvailable {
                reason: "minimum stake not met".into(),
            });
        }

        inner.mining_state = MiningState::Mining;
        info!("FFI: mining started");

        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: inner.pol.current_day_valid(),
            presence_score: inner.presence_score,
        })
    }

    /// Stop mining.
    ///
    /// Returns the updated mining status. The node reverts to Proof of Life
    /// passive collection mode.
    pub fn stop_mining(&self) -> Result<FfiMiningStatus, FfiError> {
        let mut inner = self.lock_inner()?;
        inner.mining_state = MiningState::ProofOfLife;
        info!("FFI: mining stopped");

        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: inner.pol.current_day_valid(),
            presence_score: inner.presence_score,
        })
    }

    // ========================================================================
    // Proof of Life methods
    // ========================================================================

    /// Get the current Proof of Life status.
    pub fn get_proof_of_life_status(&self) -> Result<FfiProofOfLifeStatus, FfiError> {
        let inner = self.lock_inner()?;
        let daily_data = inner.sensor_buffer.to_daily_data();

        // Build list of which PoL parameters are currently satisfied.
        let mut params_met = Vec::new();
        if daily_data.unlock_count >= 10 {
            params_met.push("unlocks".to_string());
        }
        // Check unlock spread
        if let (Some(first), Some(last)) = (daily_data.first_unlock, daily_data.last_unlock) {
            if (last - first).num_hours() >= 6 {
                params_met.push("unlock_spread".to_string());
            }
        }
        if daily_data.interaction_sessions >= 3 {
            params_met.push("interactions".to_string());
        }
        if daily_data.orientation_changed {
            params_met.push("orientation".to_string());
        }
        if daily_data.human_motion_detected {
            params_met.push("motion".to_string());
        }
        if daily_data.gps_fix_obtained {
            params_met.push("gps".to_string());
        }
        if daily_data.distinct_wifi_networks >= 1 || daily_data.distinct_bt_environments >= 1 {
            params_met.push("network".to_string());
        }
        if daily_data.distinct_bt_environments >= 2 {
            params_met.push("bt_variation".to_string());
        }
        if daily_data.charge_cycle_event {
            params_met.push("charge_event".to_string());
        }

        Ok(FfiProofOfLifeStatus {
            is_valid_today: inner.pol.current_day_valid(),
            consecutive_days: inner.pol.consecutive_days(),
            is_onboarded: inner.pol.is_onboarded(),
            parameters_met: params_met,
        })
    }

    /// Submit a sensor event from the native platform layer.
    ///
    /// Called by the Android/iOS sensor managers whenever a relevant event
    /// occurs (unlock, GPS fix, BT scan, etc.). Events are buffered and
    /// processed into the daily PoL attestation.
    pub fn submit_sensor_event(&self, event: FfiSensorEvent) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;
        let internal_event: gratia_pol::collector::SensorEvent = event.into();
        inner.sensor_buffer.process_event(internal_event);
        Ok(())
    }

    /// Finalize the current day's Proof of Life.
    ///
    /// Called at end-of-day (midnight UTC). Evaluates all accumulated sensor
    /// data, generates the PoL attestation, and resets the sensor buffer.
    ///
    /// Returns `true` if the day was valid (all PoL parameters met).
    pub fn finalize_day(&self) -> Result<bool, FfiError> {
        let mut inner = self.lock_inner()?;

        // Feed the buffered sensor data into the PoL manager.
        let daily_data = inner.sensor_buffer.to_daily_data();

        // WHY: We replay the daily data into the PoL manager's individual
        // record methods to keep its internal state consistent. This is the
        // bridge between the event-based sensor buffer and the PoL manager's
        // record-based API.
        if daily_data.unlock_count > 0 {
            for _ in 0..daily_data.unlock_count {
                inner.pol.record_unlock();
            }
        }
        for _ in 0..daily_data.interaction_sessions {
            inner.pol.record_interaction_session();
        }
        if daily_data.orientation_changed {
            inner.pol.record_orientation_change();
        }
        if daily_data.human_motion_detected {
            inner.pol.record_human_motion();
        }
        if daily_data.gps_fix_obtained {
            if let Some(loc) = daily_data.approximate_location {
                inner.pol.record_gps_fix(loc.lat, loc.lon);
            }
        }
        for _ in 0..daily_data.distinct_wifi_networks {
            inner.pol.record_wifi_network();
        }
        for _ in 0..daily_data.distinct_bt_environments {
            inner.pol.record_bt_environment_change();
        }
        if daily_data.charge_cycle_event {
            inner.pol.record_charge_event();
        }

        let is_valid = inner.pol.finalize_day();

        // Reset the sensor buffer for the new day.
        inner.sensor_buffer.reset();

        if is_valid {
            info!("FFI: day finalized — VALID");
            // Record PoL event for wallet's dead-man switch (inheritance).
            inner.wallet.record_proof_of_life();
        } else {
            warn!("FFI: day finalized — INVALID");
        }

        Ok(is_valid)
    }

    // ========================================================================
    // Staking methods
    // ========================================================================

    /// Stake GRAT for mining eligibility.
    ///
    /// `amount` is in Lux. If the total committed stake exceeds the per-node
    /// cap, the excess automatically flows to the Network Security Pool.
    ///
    /// Returns the transaction hash as a hex string.
    pub fn stake(&self, amount: u64) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: Placeholder fee of 1000 Lux. Same rationale as send_transfer.
        let fee: u64 = 1000;

        let tx = inner.wallet.send_stake(amount, fee)?;
        let hash_hex = hex::encode(tx.hash.0);

        // Also register the stake in the local staking manager.
        let node_id = self.get_node_id_or_default(&inner);
        if let Err(e) = inner.staking.stake(node_id, amount, Utc::now()) {
            error!("FFI: staking manager error: {}", e);
            return Err(FfiError::StakingError {
                reason: e.to_string(),
            });
        }

        info!("FFI: stake of {} Lux sent, hash={}", amount, hash_hex);
        Ok(hash_hex)
    }

    /// Unstake GRAT (subject to cooldown period).
    ///
    /// `amount` is in Lux. Overflow stake is removed first to preserve
    /// consensus participation.
    ///
    /// Returns the transaction hash as a hex string.
    pub fn unstake(&self, amount: u64) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        let fee: u64 = 1000; // Placeholder fee

        let tx = inner.wallet.send_unstake(amount, fee)?;
        let hash_hex = hex::encode(tx.hash.0);

        let node_id = self.get_node_id_or_default(&inner);
        if let Err(e) = inner.staking.request_unstake(node_id, amount, Utc::now()) {
            error!("FFI: staking manager unstake error: {}", e);
            return Err(FfiError::StakingError {
                reason: e.to_string(),
            });
        }

        info!("FFI: unstake of {} Lux sent, hash={}", amount, hash_hex);
        Ok(hash_hex)
    }

    /// Get current staking information for this node.
    pub fn get_stake_info(&self) -> Result<FfiStakeInfo, FfiError> {
        let inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);

        match inner.staking.get_stake_info(&node_id) {
            Some(info) => Ok(FfiStakeInfo::from(&info)),
            None => {
                // No stake exists — return zeroed info.
                Ok(FfiStakeInfo {
                    node_stake_lux: 0,
                    overflow_amount_lux: 0,
                    total_committed_lux: 0,
                    staked_at_millis: 0,
                    meets_minimum: false,
                })
            }
        }
    }
}

// ============================================================================
// Private helpers (not exported via UniFFI)
// ============================================================================

impl GratiaNode {
    /// Acquire the inner mutex, mapping poisoned lock to FfiError.
    fn lock_inner(&self) -> Result<std::sync::MutexGuard<'_, GratiaNodeInner>, FfiError> {
        self.inner.lock().map_err(|e| {
            error!("FFI: mutex poisoned: {}", e);
            FfiError::InternalError {
                reason: "internal lock error — please restart the app".into(),
            }
        })
    }

    /// Get the NodeId for the current wallet, or a zeroed default if the wallet
    /// is not yet initialized.
    ///
    /// WHY: Several subsystems (staking, mining state) need a NodeId to look up
    /// per-node records. Before the wallet is created, we return a zeroed NodeId
    /// which will not match any staking record — this is safe because staking
    /// and mining are impossible without a wallet anyway.
    fn get_node_id_or_default(
        &self,
        inner: &GratiaNodeInner,
    ) -> gratia_core::types::NodeId {
        inner
            .wallet
            .address()
            .map(|addr| {
                // WHY: We reuse the address bytes as a NodeId for local lookups.
                // In production, the NodeId is derived from the public key via
                // NodeId::from_public_key(), but at the FFI layer we don't have
                // direct access to the VerifyingKey. The address bytes serve as
                // a unique identifier for local staking manager lookups.
                gratia_core::types::NodeId(addr.0)
            })
            .unwrap_or(gratia_core::types::NodeId([0u8; 32]))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node() -> GratiaNode {
        GratiaNode::new("/tmp/gratia-test".to_string())
    }

    #[test]
    fn test_create_node() {
        let node = test_node();
        assert_eq!(node.data_dir, "/tmp/gratia-test");
    }

    #[test]
    fn test_create_wallet() {
        let node = test_node();
        let addr = node.create_wallet().unwrap();
        assert!(addr.starts_with("grat:"));
        assert_eq!(addr.len(), 5 + 64); // "grat:" + 64 hex chars
    }

    #[test]
    fn test_create_wallet_twice_fails() {
        let node = test_node();
        node.create_wallet().unwrap();
        let result = node.create_wallet();
        assert!(result.is_err());
    }

    #[test]
    fn test_get_wallet_info_before_create() {
        let node = test_node();
        let result = node.get_wallet_info();
        assert!(result.is_err());
    }

    #[test]
    fn test_get_wallet_info_after_create() {
        let node = test_node();
        node.create_wallet().unwrap();
        let info = node.get_wallet_info().unwrap();
        assert!(info.address.starts_with("grat:"));
        assert_eq!(info.balance_lux, 0);
        assert_eq!(info.mining_state, "proof_of_life");
    }

    #[test]
    fn test_mining_status_defaults() {
        let node = test_node();
        let status = node.get_mining_status().unwrap();
        assert_eq!(status.state, "proof_of_life");
        assert!(!status.is_plugged_in);
        assert_eq!(status.battery_percent, 0);
        assert!(!status.current_day_pol_valid);
    }

    #[test]
    fn test_update_power_state() {
        let node = test_node();
        let status = node.update_power_state(true, 85).unwrap();
        assert!(status.is_plugged_in);
        assert_eq!(status.battery_percent, 85);
    }

    #[test]
    fn test_start_mining_without_conditions() {
        let node = test_node();
        // Not plugged in — should fail.
        let result = node.start_mining();
        assert!(result.is_err());
    }

    #[test]
    fn test_stop_mining() {
        let node = test_node();
        let status = node.stop_mining().unwrap();
        assert_eq!(status.state, "proof_of_life");
    }

    #[test]
    fn test_submit_sensor_events() {
        let node = test_node();
        node.submit_sensor_event(FfiSensorEvent::Unlock).unwrap();
        node.submit_sensor_event(FfiSensorEvent::Motion).unwrap();
        node.submit_sensor_event(FfiSensorEvent::GpsUpdate {
            lat: 40.7,
            lon: -74.0,
        })
        .unwrap();
        node.submit_sensor_event(FfiSensorEvent::ChargeEvent { is_charging: true })
            .unwrap();

        // Events should be buffered — PoL status should reflect them.
        let status = node.get_proof_of_life_status().unwrap();
        assert!(status.parameters_met.contains(&"motion".to_string()));
        assert!(status.parameters_met.contains(&"gps".to_string()));
        assert!(status.parameters_met.contains(&"charge_event".to_string()));
    }

    #[test]
    fn test_finalize_day_invalid() {
        let node = test_node();
        // No sensor events submitted — day should be invalid.
        let result = node.finalize_day().unwrap();
        assert!(!result);
    }

    #[test]
    fn test_get_stake_info_no_stake() {
        let node = test_node();
        node.create_wallet().unwrap();
        let info = node.get_stake_info().unwrap();
        assert_eq!(info.node_stake_lux, 0);
        assert!(!info.meets_minimum);
    }

    #[test]
    fn test_proof_of_life_status_initial() {
        let node = test_node();
        let status = node.get_proof_of_life_status().unwrap();
        assert!(!status.is_valid_today);
        assert_eq!(status.consecutive_days, 0);
        assert!(!status.is_onboarded);
        assert!(status.parameters_met.is_empty());
    }

    #[test]
    fn test_send_transfer_no_balance() {
        let node = test_node();
        node.create_wallet().unwrap();
        let recipient = "grat:".to_string() + &hex::encode([0x42u8; 32]);
        let result = node.send_transfer(recipient, 1_000_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_ffi_error_display() {
        let err = FfiError::WalletNotInitialized;
        let msg = err.to_string();
        assert!(msg.contains("create_wallet"));
    }
}
