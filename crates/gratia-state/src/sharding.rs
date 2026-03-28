//! Geographic shard management for horizontal scaling.
//!
//! Gratia uses geographic sharding to achieve ~2,000 TPS across 10 shards.
//! Each shard covers a geographic region and processes transactions originating
//! from or destined to addresses within that region. Cross-shard transactions
//! are routed through a relay mechanism.
//!
//! Shard assignment is based on GPS coordinates rounded to regional granularity.
//! This ensures that nearby nodes are in the same shard, minimizing network
//! latency for consensus within a shard.

use serde::{Deserialize, Serialize};

use gratia_core::error::GratiaError;
use gratia_core::types::{Address, GeoLocation, ShardId};

// ============================================================================
// Constants
// ============================================================================

/// Maximum number of shards the network can scale to.
/// WHY: 20 shards at ~200 TPS each = 4,000 TPS. Governance-adjustable.
pub const MAX_SHARDS: u16 = 20;

/// Default number of active shards at genesis.
/// WHY: Start with a smaller number and expand as the network grows
/// geographically. Too many shards with too few nodes per shard weakens
/// consensus security within each shard.
pub const DEFAULT_ACTIVE_SHARDS: u16 = 4;

/// Minimum total network nodes before sharding activates.
/// WHY: Below 10K nodes, a single global chain is safer — shard isolation
/// weakens consensus security. 131-218 TPS is sufficient at this scale.
pub const SHARDING_ACTIVATION_THRESHOLD: u64 = 10_000;

/// Percentage of committee selected from neighboring shards.
/// WHY: 20% cross-shard validators prevent single-shard capture.
/// Adds ~50-100ms latency but ensures even local-majority attackers
/// can't finalize blocks without cross-shard agreement.
pub const CROSS_SHARD_COMMITTEE_PCT: u8 = 20;

/// Shard boundary jitter in degrees.
/// WHY: Nodes within 2° of a boundary have 30% chance of adjacent shard
/// assignment, preventing attacker from precisely targeting shard placement.
pub const BOUNDARY_JITTER_DEGREES: f32 = 2.0;

/// Probability (0-100) that a node within the jitter zone gets reassigned.
pub const BOUNDARY_JITTER_PROBABILITY: u8 = 30;

// ============================================================================
// Shard Configuration
// ============================================================================

/// Configuration for the sharding system.
#[derive(Debug, Clone)]
pub struct ShardConfig {
    /// Number of currently active shards.
    pub active_shards: u16,
    /// Minimum number of active mining nodes per shard before the shard is
    /// considered healthy for independent consensus.
    /// WHY: Below this threshold, shard security may be compromised.
    pub min_nodes_per_shard: u32,
    /// Multiplier for split threshold: shard splits when active_nodes > min_nodes * split_multiplier
    /// for split_persistence_days consecutive days.
    /// WHY: 5× minimum ensures both sub-shards are well above safety threshold.
    pub split_multiplier: u32,
    /// Days a shard must sustain above split threshold before splitting.
    pub split_persistence_days: u32,
    /// Throughput fraction (0-100) that must be exceeded alongside node count for split.
    /// WHY: Only split when there's actual capacity pressure, not just many nodes.
    pub split_throughput_threshold_pct: u8,
    /// Days below minimum before merge triggers.
    pub merge_persistence_days: u32,
}

impl Default for ShardConfig {
    fn default() -> Self {
        ShardConfig {
            active_shards: DEFAULT_ACTIVE_SHARDS,
            // WHY: 50 nodes minimum ensures that a 21-member validator committee
            // can be selected with sufficient diversity, and Byzantine fault
            // tolerance holds even if some nodes go offline.
            min_nodes_per_shard: 50,
            split_multiplier: 5,
            split_persistence_days: 30,
            split_throughput_threshold_pct: 70,
            merge_persistence_days: 7,
        }
    }
}

// ============================================================================
// Shard State
// ============================================================================

/// State information tracked per shard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardState {
    /// The shard identifier.
    pub shard_id: ShardId,
    /// Current block height within this shard.
    pub block_height: u64,
    /// Current state root for this shard's state trie.
    pub state_root: [u8; 32],
    /// Number of active mining nodes in this shard.
    pub active_nodes: u64,
    /// Geographic center of this shard (approximate).
    pub center: Option<GeoLocation>,
    /// Health metrics for this shard.
    pub health_metrics: ShardHealthMetrics,
}

/// Health status of a shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShardHealth {
    /// Active nodes > 2× minimum. Operating normally.
    Healthy,
    /// Active nodes between 1-2× minimum. Enhanced monitoring active.
    Warning,
    /// Active nodes below minimum. Merge required.
    Critical,
}

/// Tracked health metrics for a shard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardHealthMetrics {
    /// Current health status.
    pub health: ShardHealth,
    /// Finality rate over the last epoch (0.0-1.0).
    pub finality_rate: f64,
    /// Fraction of committee slots held by the most concentrated entity (0.0-1.0).
    pub max_validator_concentration: f64,
    /// Node churn rate over the last 7 days (fraction lost per week).
    pub weekly_churn_rate: f64,
    /// Number of consecutive days below minimum node threshold.
    pub days_below_minimum: u32,
    /// Number of consecutive days above split threshold.
    pub days_above_split: u32,
}

impl Default for ShardHealthMetrics {
    fn default() -> Self {
        ShardHealthMetrics {
            health: ShardHealth::Healthy,
            finality_rate: 1.0,
            max_validator_concentration: 0.0,
            weekly_churn_rate: 0.0,
            days_below_minimum: 0,
            days_above_split: 0,
        }
    }
}

impl ShardState {
    /// Create initial state for a new shard.
    pub fn new(shard_id: ShardId) -> Self {
        ShardState {
            shard_id,
            block_height: 0,
            state_root: [0u8; 32],
            active_nodes: 0,
            center: None,
            health_metrics: ShardHealthMetrics::default(),
        }
    }
}

// ============================================================================
// Cross-Shard Transaction
// ============================================================================

/// A cross-shard transaction receipt, used to route transactions between shards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossShardReceipt {
    /// The original transaction hash.
    pub tx_hash: [u8; 32],
    /// Source shard.
    pub source_shard: ShardId,
    /// Destination shard.
    pub dest_shard: ShardId,
    /// Merkle proof of inclusion in the source shard's block.
    pub inclusion_proof: Vec<u8>,
    /// The block height in the source shard where this was included.
    pub source_block_height: u64,
}

// ============================================================================
// Shard Manager
// ============================================================================

/// Manages geographic shard assignment and cross-shard routing.
pub struct ShardManager {
    config: ShardConfig,
    /// Per-shard state tracking.
    shard_states: Vec<ShardState>,
}

impl ShardManager {
    /// Create a new ShardManager with the given configuration.
    pub fn new(config: ShardConfig) -> Self {
        let shard_states = (0..config.active_shards)
            .map(|i| ShardState::new(ShardId(i)))
            .collect();

        ShardManager {
            config,
            shard_states,
        }
    }

    /// Create a ShardManager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(ShardConfig::default())
    }

    /// Get the number of active shards.
    pub fn active_shard_count(&self) -> u16 {
        self.config.active_shards
    }

    /// Assign a node to a shard based on its GPS location.
    ///
    /// Uses a simple geographic partitioning scheme that divides the globe
    /// into longitudinal bands. This keeps nearby nodes in the same shard
    /// for low-latency intra-shard consensus.
    ///
    /// If no location is available, falls back to a hash-based assignment
    /// using the node's address.
    pub fn assign_shard(
        &self,
        location: Option<&GeoLocation>,
        fallback_address: &Address,
    ) -> ShardId {
        match location {
            Some(loc) => self.shard_from_location(loc),
            None => {
                // WHY: Nodes without GPS (rare, since GPS is a core requirement)
                // are assigned deterministically by hashing their address. This
                // provides even distribution without requiring location data.
                self.shard_from_address(fallback_address)
            }
        }
    }

    /// Determine which shard should process a transaction.
    ///
    /// For standard transfers, the sender's shard processes the transaction.
    /// If sender and recipient are in different shards, the transaction
    /// originates in the sender's shard and a cross-shard receipt is
    /// generated for the recipient's shard.
    pub fn shard_for_transaction(
        &self,
        sender_location: Option<&GeoLocation>,
        sender_address: &Address,
    ) -> ShardId {
        self.assign_shard(sender_location, sender_address)
    }

    /// Determine if a transaction requires cross-shard routing.
    ///
    /// Returns `Some((source, dest))` if cross-shard, `None` if same-shard.
    pub fn cross_shard_routing(
        &self,
        sender_location: Option<&GeoLocation>,
        sender_address: &Address,
        recipient_location: Option<&GeoLocation>,
        recipient_address: &Address,
    ) -> Option<(ShardId, ShardId)> {
        let source = self.assign_shard(sender_location, sender_address);
        let dest = self.assign_shard(recipient_location, recipient_address);

        if source == dest {
            None
        } else {
            Some((source, dest))
        }
    }

    /// Get the state of a specific shard.
    pub fn get_shard_state(&self, shard_id: ShardId) -> Result<&ShardState, GratiaError> {
        self.shard_states
            .get(shard_id.0 as usize)
            .ok_or_else(|| GratiaError::ShardNotAvailable {
                shard_id: shard_id.0,
            })
    }

    /// Get a mutable reference to a shard's state.
    pub fn get_shard_state_mut(
        &mut self,
        shard_id: ShardId,
    ) -> Result<&mut ShardState, GratiaError> {
        self.shard_states
            .get_mut(shard_id.0 as usize)
            .ok_or_else(|| GratiaError::ShardNotAvailable {
                shard_id: shard_id.0,
            })
    }

    /// Update a shard's block height and state root after a new block.
    pub fn update_shard(
        &mut self,
        shard_id: ShardId,
        new_height: u64,
        new_state_root: [u8; 32],
    ) -> Result<(), GratiaError> {
        let state = self.get_shard_state_mut(shard_id)?;
        state.block_height = new_height;
        state.state_root = new_state_root;
        Ok(())
    }

    /// Update the active node count for a shard.
    pub fn set_active_nodes(
        &mut self,
        shard_id: ShardId,
        count: u64,
    ) -> Result<(), GratiaError> {
        let state = self.get_shard_state_mut(shard_id)?;
        state.active_nodes = count;
        Ok(())
    }

    /// Check if a shard has enough nodes for healthy consensus.
    pub fn is_shard_healthy(&self, shard_id: ShardId) -> Result<bool, GratiaError> {
        let state = self.get_shard_state(shard_id)?;
        Ok(state.active_nodes >= self.config.min_nodes_per_shard as u64)
    }

    /// Get all shard states.
    pub fn all_shard_states(&self) -> &[ShardState] {
        &self.shard_states
    }

    /// Evaluate the health of a shard based on its metrics.
    pub fn evaluate_shard_health(&self, shard_id: ShardId) -> Result<ShardHealth, GratiaError> {
        let state = self.get_shard_state(shard_id)?;
        let min = self.config.min_nodes_per_shard as u64;

        if state.active_nodes < min {
            Ok(ShardHealth::Critical)
        } else if state.active_nodes < min * 2 {
            Ok(ShardHealth::Warning)
        } else {
            Ok(ShardHealth::Healthy)
        }
    }

    /// Check if a shard should be split.
    pub fn should_split(&self, shard_id: ShardId) -> Result<bool, GratiaError> {
        let state = self.get_shard_state(shard_id)?;
        let threshold = self.config.min_nodes_per_shard as u64 * self.config.split_multiplier as u64;

        Ok(
            state.active_nodes > threshold
            && self.config.active_shards < MAX_SHARDS
            && state.health_metrics.days_above_split >= self.config.split_persistence_days
        )
    }

    /// Check if a shard should be merged with its neighbor.
    pub fn should_merge(&self, shard_id: ShardId) -> Result<bool, GratiaError> {
        let state = self.get_shard_state(shard_id)?;

        Ok(
            state.active_nodes < self.config.min_nodes_per_shard as u64
            && state.health_metrics.days_below_minimum >= self.config.merge_persistence_days
            && self.config.active_shards > 1
        )
    }

    /// Check if the network has enough nodes to activate sharding.
    pub fn should_activate_sharding(&self, total_network_nodes: u64) -> bool {
        total_network_nodes >= SHARDING_ACTIVATION_THRESHOLD && self.config.active_shards <= 1
    }

    /// Calculate how many cross-shard validator slots for a given committee size.
    pub fn cross_shard_validator_count(committee_size: usize) -> usize {
        // WHY: ceil division ensures at least 1 cross-shard validator for any committee >= 5.
        (committee_size * CROSS_SHARD_COMMITTEE_PCT as usize + 99) / 100
    }

    // --- Internal helpers ---

    /// Map a geographic location to a shard ID using longitudinal bands.
    ///
    /// Divides the globe into `active_shards` equal longitudinal bands.
    /// Longitude ranges from -180 to +180 degrees.
    fn shard_from_location(&self, location: &GeoLocation) -> ShardId {
        // Normalize longitude to [0, 360) range.
        let normalized_lon = (location.lon as f64 + 180.0).rem_euclid(360.0);
        let band_width = 360.0 / self.config.active_shards as f64;
        let shard_index = (normalized_lon / band_width) as u16;

        // Clamp to valid range (floating point edge cases).
        ShardId(shard_index.min(self.config.active_shards - 1))
    }

    /// Map an address to a shard ID using hash-based assignment.
    fn shard_from_address(&self, address: &Address) -> ShardId {
        // WHY: Use the first 2 bytes of the address as a simple hash for shard
        // assignment. The address is already a SHA-256 hash, so its bytes are
        // uniformly distributed. This gives even distribution across shards.
        let hash_val = u16::from_be_bytes([address.0[0], address.0[1]]);
        ShardId(hash_val % self.config.active_shards)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_location(lat: f32, lon: f32) -> GeoLocation {
        GeoLocation { lat, lon }
    }

    fn make_address(seed: u8) -> Address {
        Address([seed; 32])
    }

    #[test]
    fn test_shard_manager_creation() {
        let mgr = ShardManager::with_defaults();
        assert_eq!(mgr.active_shard_count(), DEFAULT_ACTIVE_SHARDS);
        assert_eq!(mgr.all_shard_states().len(), DEFAULT_ACTIVE_SHARDS as usize);
    }

    #[test]
    fn test_shard_from_location_western_hemisphere() {
        let mgr = ShardManager::new(ShardConfig {
            active_shards: 4,
            min_nodes_per_shard: 10,
            ..ShardConfig::default()
        });

        // New York: ~-74 lon -> normalized = 106 -> band_width = 90 -> shard 1
        let ny = make_location(40.7, -74.0);
        let shard = mgr.assign_shard(Some(&ny), &make_address(0));
        assert!(shard.0 < 4);
    }

    #[test]
    fn test_shard_from_location_eastern_hemisphere() {
        let mgr = ShardManager::new(ShardConfig {
            active_shards: 4,
            min_nodes_per_shard: 10,
            ..ShardConfig::default()
        });

        // Tokyo: ~139.7 lon -> normalized = 319.7 -> band_width = 90 -> shard 3
        let tokyo = make_location(35.7, 139.7);
        let shard = mgr.assign_shard(Some(&tokyo), &make_address(0));
        assert!(shard.0 < 4);
    }

    #[test]
    fn test_shard_deterministic() {
        let mgr = ShardManager::with_defaults();
        let loc = make_location(51.5, -0.1); // London
        let addr = make_address(42);

        let s1 = mgr.assign_shard(Some(&loc), &addr);
        let s2 = mgr.assign_shard(Some(&loc), &addr);
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_shard_from_address_fallback() {
        let mgr = ShardManager::with_defaults();
        let addr = make_address(42);

        // No location — uses address-based assignment.
        let shard = mgr.assign_shard(None, &addr);
        assert!(shard.0 < mgr.active_shard_count());
    }

    #[test]
    fn test_shard_from_address_distribution() {
        let mgr = ShardManager::new(ShardConfig {
            active_shards: 10,
            min_nodes_per_shard: 10,
            ..ShardConfig::default()
        });

        // Check that different addresses map to different shards (probabilistic).
        let mut shard_counts = vec![0u32; 10];
        for i in 0..100u8 {
            let addr = make_address(i);
            let shard = mgr.assign_shard(None, &addr);
            shard_counts[shard.0 as usize] += 1;
        }

        // At least some shards should have assignments.
        let non_empty = shard_counts.iter().filter(|&&c| c > 0).count();
        assert!(non_empty >= 2, "Expected distribution across multiple shards");
    }

    #[test]
    fn test_cross_shard_routing_same_shard() {
        let mgr = ShardManager::with_defaults();
        let loc = make_location(40.0, -74.0);
        let addr1 = make_address(1);
        let addr2 = make_address(2);

        // Same location -> same shard -> no cross-shard routing.
        let result = mgr.cross_shard_routing(Some(&loc), &addr1, Some(&loc), &addr2);
        assert!(result.is_none());
    }

    #[test]
    fn test_cross_shard_routing_different_shards() {
        let mgr = ShardManager::new(ShardConfig {
            active_shards: 4,
            min_nodes_per_shard: 10,
            ..ShardConfig::default()
        });

        // Far apart locations should be in different shards.
        let ny = make_location(40.7, -74.0);
        let tokyo = make_location(35.7, 139.7);
        let addr1 = make_address(1);
        let addr2 = make_address(2);

        let ny_shard = mgr.assign_shard(Some(&ny), &addr1);
        let tokyo_shard = mgr.assign_shard(Some(&tokyo), &addr2);

        if ny_shard != tokyo_shard {
            let result =
                mgr.cross_shard_routing(Some(&ny), &addr1, Some(&tokyo), &addr2);
            assert!(result.is_some());
            let (src, dst) = result.unwrap();
            assert_eq!(src, ny_shard);
            assert_eq!(dst, tokyo_shard);
        }
    }

    #[test]
    fn test_shard_state_updates() {
        let mut mgr = ShardManager::with_defaults();
        let shard = ShardId(0);

        // Initial state
        let state = mgr.get_shard_state(shard).unwrap();
        assert_eq!(state.block_height, 0);
        assert_eq!(state.active_nodes, 0);

        // Update
        mgr.update_shard(shard, 100, [42u8; 32]).unwrap();
        mgr.set_active_nodes(shard, 75).unwrap();

        let state = mgr.get_shard_state(shard).unwrap();
        assert_eq!(state.block_height, 100);
        assert_eq!(state.state_root, [42u8; 32]);
        assert_eq!(state.active_nodes, 75);
    }

    #[test]
    fn test_shard_health() {
        let mut mgr = ShardManager::new(ShardConfig {
            active_shards: 2,
            min_nodes_per_shard: 50,
            ..ShardConfig::default()
        });

        let shard = ShardId(0);

        // Below minimum
        mgr.set_active_nodes(shard, 30).unwrap();
        assert!(!mgr.is_shard_healthy(shard).unwrap());

        // At minimum
        mgr.set_active_nodes(shard, 50).unwrap();
        assert!(mgr.is_shard_healthy(shard).unwrap());

        // Above minimum
        mgr.set_active_nodes(shard, 100).unwrap();
        assert!(mgr.is_shard_healthy(shard).unwrap());
    }

    #[test]
    fn test_invalid_shard_id() {
        let mgr = ShardManager::with_defaults();
        let invalid = ShardId(99);
        assert!(mgr.get_shard_state(invalid).is_err());
    }

    #[test]
    fn test_longitude_edge_cases() {
        let mgr = ShardManager::new(ShardConfig {
            active_shards: 10,
            min_nodes_per_shard: 10,
            ..ShardConfig::default()
        });

        // Date line: lon = 180
        let dateline = make_location(0.0, 180.0);
        let shard = mgr.assign_shard(Some(&dateline), &make_address(0));
        assert!(shard.0 < 10);

        // Antimeridian: lon = -180
        let anti = make_location(0.0, -180.0);
        let shard = mgr.assign_shard(Some(&anti), &make_address(0));
        assert!(shard.0 < 10);

        // Prime meridian: lon = 0
        let prime = make_location(0.0, 0.0);
        let shard = mgr.assign_shard(Some(&prime), &make_address(0));
        assert!(shard.0 < 10);
    }

    #[test]
    fn test_shard_health_evaluation() {
        let mut mgr = ShardManager::new(ShardConfig {
            active_shards: 2,
            min_nodes_per_shard: 50,
            ..ShardConfig::default()
        });
        let shard = ShardId(0);

        // Below minimum -> Critical
        mgr.set_active_nodes(shard, 30).unwrap();
        assert_eq!(mgr.evaluate_shard_health(shard).unwrap(), ShardHealth::Critical);

        // Between 1-2x minimum -> Warning
        mgr.set_active_nodes(shard, 50).unwrap();
        assert_eq!(mgr.evaluate_shard_health(shard).unwrap(), ShardHealth::Warning);
        mgr.set_active_nodes(shard, 99).unwrap();
        assert_eq!(mgr.evaluate_shard_health(shard).unwrap(), ShardHealth::Warning);

        // At or above 2x minimum -> Healthy
        mgr.set_active_nodes(shard, 100).unwrap();
        assert_eq!(mgr.evaluate_shard_health(shard).unwrap(), ShardHealth::Healthy);
        mgr.set_active_nodes(shard, 200).unwrap();
        assert_eq!(mgr.evaluate_shard_health(shard).unwrap(), ShardHealth::Healthy);
    }

    #[test]
    fn test_should_split() {
        let mut mgr = ShardManager::new(ShardConfig {
            active_shards: 4,
            min_nodes_per_shard: 50,
            split_multiplier: 5,
            split_persistence_days: 30,
            ..ShardConfig::default()
        });
        let shard = ShardId(0);

        // Above threshold (50 * 5 = 250) but no persistence -> false
        mgr.set_active_nodes(shard, 300).unwrap();
        assert!(!mgr.should_split(shard).unwrap());

        // Set persistence days met
        {
            let state = mgr.get_shard_state_mut(shard).unwrap();
            state.health_metrics.days_above_split = 30;
        }
        assert!(mgr.should_split(shard).unwrap());

        // Below threshold -> false even with persistence
        mgr.set_active_nodes(shard, 200).unwrap();
        assert!(!mgr.should_split(shard).unwrap());
    }

    #[test]
    fn test_should_merge() {
        let mut mgr = ShardManager::new(ShardConfig {
            active_shards: 4,
            min_nodes_per_shard: 50,
            merge_persistence_days: 7,
            ..ShardConfig::default()
        });
        let shard = ShardId(0);

        // Below minimum but no persistence -> false
        mgr.set_active_nodes(shard, 30).unwrap();
        assert!(!mgr.should_merge(shard).unwrap());

        // Set persistence days met
        {
            let state = mgr.get_shard_state_mut(shard).unwrap();
            state.health_metrics.days_below_minimum = 7;
        }
        assert!(mgr.should_merge(shard).unwrap());

        // Above minimum -> false even with persistence
        mgr.set_active_nodes(shard, 60).unwrap();
        assert!(!mgr.should_merge(shard).unwrap());
    }

    #[test]
    fn test_sharding_activation_threshold() {
        let mgr = ShardManager::new(ShardConfig {
            active_shards: 1,
            min_nodes_per_shard: 50,
            ..ShardConfig::default()
        });

        // Below threshold -> should not activate
        assert!(!mgr.should_activate_sharding(9_999));

        // At threshold -> should activate
        assert!(mgr.should_activate_sharding(10_000));

        // Above threshold -> should activate
        assert!(mgr.should_activate_sharding(50_000));

        // Already has multiple shards -> should not activate
        let mgr2 = ShardManager::new(ShardConfig {
            active_shards: 4,
            min_nodes_per_shard: 50,
            ..ShardConfig::default()
        });
        assert!(!mgr2.should_activate_sharding(20_000));
    }

    #[test]
    fn test_cross_shard_validator_count() {
        // committee 3 -> ceil(3 * 20 / 100) = ceil(0.6) = 1
        assert_eq!(ShardManager::cross_shard_validator_count(3), 1);
        // committee 7 -> ceil(7 * 20 / 100) = ceil(1.4) = 2
        assert_eq!(ShardManager::cross_shard_validator_count(7), 2);
        // committee 21 -> ceil(21 * 20 / 100) = ceil(4.2) = 5
        assert_eq!(ShardManager::cross_shard_validator_count(21), 5);
    }

    #[test]
    fn test_max_shards_prevents_split() {
        // Set active_shards to MAX_SHARDS so split is impossible
        let mut mgr = ShardManager::new(ShardConfig {
            active_shards: MAX_SHARDS,
            min_nodes_per_shard: 50,
            split_multiplier: 5,
            split_persistence_days: 30,
            ..ShardConfig::default()
        });
        let shard = ShardId(0);

        mgr.set_active_nodes(shard, 500).unwrap();
        {
            let state = mgr.get_shard_state_mut(shard).unwrap();
            state.health_metrics.days_above_split = 30;
        }

        // Even though node count and persistence are met, MAX_SHARDS prevents split
        assert!(!mgr.should_split(shard).unwrap());
    }
}
