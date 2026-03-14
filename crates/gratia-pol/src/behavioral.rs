//! Behavioral pattern matching — used for wallet recovery verification.
//!
//! When a user loses their device and needs to recover their wallet on a new
//! phone, the protocol requires them to use the new phone normally for 7-14
//! days. The accumulated behavioral patterns are then compared against the
//! stored profile from the original device.
//!
//! Key behavioral dimensions:
//! - **Unlock timing patterns:** When during the day the user unlocks their phone
//! - **Interaction frequency:** How often and how long interaction sessions are
//! - **Movement patterns:** GPS location habits (coarse — home/work/commute)
//! - **Location patterns:** Which general areas the user frequents
//! - **Charging habits:** When the user typically charges their phone
//!
//! PRIVACY: The profile is a statistical summary — it cannot reconstruct
//! specific locations, times, or activities. All raw data is discarded
//! after profile update.

use chrono::{DateTime, Utc, Timelike};
use serde::{Deserialize, Serialize};
use gratia_core::GeoLocation;

/// Number of 1-hour buckets in a day, used for time-of-day distributions.
const HOURS_IN_DAY: usize = 24;

/// Minimum number of days required to build a meaningful behavioral profile.
/// WHY: Fewer than 7 days does not capture weekly patterns (weekday vs weekend).
const MIN_PROFILE_DAYS: u32 = 7;

/// Maximum number of days to build a recovery profile.
/// WHY: After 14 days the system should have a confident match or rejection.
/// Keeping the recovery window bounded prevents indefinite wallet freezing.
const MAX_RECOVERY_DAYS: u32 = 14;

/// Minimum similarity score (0.0-1.0) to accept a behavioral match for recovery.
/// WHY: Set at 0.65 to balance security (rejecting impostors) against usability
/// (allowing for natural behavioral drift after losing a device — stress,
/// new routines, different commute while getting a replacement phone).
const DEFAULT_MATCH_THRESHOLD: f64 = 0.65;

/// A single day's behavioral observations, derived from the SensorEventBuffer
/// after end-of-day processing. This struct carries no raw sensor data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBehavioralData {
    /// Hour-of-day for each unlock event (0-23).
    pub unlock_hours: Vec<u8>,
    /// (Hour-of-day, duration_secs) for each interaction session.
    pub interaction_sessions: Vec<(u8, u32)>,
    /// Coarse GPS locations visited (rounded to ~1km).
    pub locations_visited: Vec<GeoLocation>,
    /// Hour-of-day for each charge event.
    pub charge_hours: Vec<u8>,
    /// Total number of unlocks.
    pub unlock_count: u32,
    /// Total number of interaction sessions.
    pub interaction_count: u32,
}

/// Statistical distribution over 24 hour-of-day buckets.
/// Each bucket stores a normalized frequency (0.0 to 1.0, sums to ~1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourlyDistribution {
    /// Frequency per hour bucket [0..23].
    pub buckets: [f64; HOURS_IN_DAY],
    /// Total number of samples used to build this distribution.
    pub sample_count: u64,
}

impl HourlyDistribution {
    fn new() -> Self {
        HourlyDistribution {
            buckets: [0.0; HOURS_IN_DAY],
            sample_count: 0,
        }
    }

    /// Add observations (hour values 0-23) to the distribution.
    fn add_observations(&mut self, hours: &[u8]) {
        if hours.is_empty() {
            return;
        }
        // Incrementally update the distribution using a running weighted average.
        // WHY: We maintain a normalized distribution rather than raw counts to
        // prevent long-history profiles from being rigid. Recent days' patterns
        // should carry meaningful weight.
        let new_count = self.sample_count + hours.len() as u64;
        let old_weight = self.sample_count as f64 / new_count as f64;
        let new_weight = hours.len() as f64 / new_count as f64;

        // Build a temporary distribution for the new observations
        let mut new_dist = [0.0_f64; HOURS_IN_DAY];
        for &h in hours {
            let idx = (h as usize).min(HOURS_IN_DAY - 1);
            new_dist[idx] += 1.0;
        }
        // Normalize
        let total: f64 = new_dist.iter().sum();
        if total > 0.0 {
            for v in &mut new_dist {
                *v /= total;
            }
        }

        // Merge
        for i in 0..HOURS_IN_DAY {
            self.buckets[i] = self.buckets[i] * old_weight + new_dist[i] * new_weight;
        }
        self.sample_count = new_count;
    }

    /// Compare two distributions using cosine similarity.
    /// Returns 0.0 (completely different) to 1.0 (identical).
    fn cosine_similarity(&self, other: &HourlyDistribution) -> f64 {
        if self.sample_count == 0 || other.sample_count == 0 {
            return 0.0;
        }

        let mut dot = 0.0_f64;
        let mut mag_a = 0.0_f64;
        let mut mag_b = 0.0_f64;

        for i in 0..HOURS_IN_DAY {
            dot += self.buckets[i] * other.buckets[i];
            mag_a += self.buckets[i] * self.buckets[i];
            mag_b += other.buckets[i] * other.buckets[i];
        }

        let denom = mag_a.sqrt() * mag_b.sqrt();
        if denom < 1e-12 {
            return 0.0;
        }
        (dot / denom).min(1.0)
    }
}

/// Coarse location cluster, representing a frequently visited area.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocationCluster {
    /// Centroid latitude.
    lat: f32,
    /// Centroid longitude.
    lon: f32,
    /// Number of GPS observations in this cluster.
    observation_count: u64,
}

impl LocationCluster {
    /// Distance to a point in approximate kilometers.
    /// WHY: Uses the equirectangular approximation — accurate enough at the
    /// ~1km granularity we operate at, and much cheaper than Haversine on
    /// ARM processors.
    fn distance_km(&self, lat: f32, lon: f32) -> f64 {
        let dlat = (self.lat as f64 - lat as f64).to_radians();
        let dlon = (self.lon as f64 - lon as f64).to_radians();
        let avg_lat = ((self.lat as f64 + lat as f64) / 2.0).to_radians();
        let x = dlon * avg_lat.cos();
        // WHY: 6371 km is the mean radius of Earth.
        let earth_radius_km = 6371.0;
        (dlat * dlat + x * x).sqrt() * earth_radius_km
    }

    /// Update the centroid with a new observation using a running average.
    fn add_observation(&mut self, lat: f32, lon: f32) {
        let n = self.observation_count as f64;
        let new_n = n + 1.0;
        self.lat = ((self.lat as f64 * n + lat as f64) / new_n) as f32;
        self.lon = ((self.lon as f64 * n + lon as f64) / new_n) as f32;
        self.observation_count += 1;
    }
}

/// A behavioral fingerprint accumulated over time.
///
/// This profile is stored encrypted on-device and used for wallet recovery
/// matching. It contains only statistical summaries — no raw data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehavioralProfile {
    /// When this profile was first created.
    pub created_at: DateTime<Utc>,
    /// When this profile was last updated.
    pub last_updated: DateTime<Utc>,
    /// Number of days of behavioral data incorporated.
    pub days_collected: u32,

    // --- Behavioral dimensions ---

    /// Distribution of phone unlocks by hour-of-day.
    unlock_distribution: HourlyDistribution,

    /// Distribution of interaction sessions by hour-of-day.
    interaction_distribution: HourlyDistribution,

    /// Average number of unlocks per day.
    avg_daily_unlocks: f64,

    /// Average number of interaction sessions per day.
    avg_daily_interactions: f64,

    /// Average interaction session duration in seconds.
    avg_session_duration_secs: f64,

    /// Distribution of charging events by hour-of-day.
    charge_distribution: HourlyDistribution,

    /// Coarse location clusters the user frequents (max 10).
    /// WHY: Capped at 10 to bound storage and because most people have
    /// a small number of regular locations (home, work, 1-3 others).
    location_clusters: Vec<LocationCluster>,
}

/// Maximum number of location clusters to track per profile.
/// WHY: Most humans frequent fewer than 10 distinct locations regularly.
/// Bounding this prevents unbounded memory growth on mobile devices.
const MAX_LOCATION_CLUSTERS: usize = 10;

/// Radius in km within which GPS observations merge into an existing cluster.
/// WHY: 2km provides enough resolution to distinguish home from work
/// while being coarse enough to preserve privacy.
const CLUSTER_MERGE_RADIUS_KM: f64 = 2.0;

impl BehavioralProfile {
    /// Create a new empty profile.
    pub fn new() -> Self {
        let now = Utc::now();
        BehavioralProfile {
            created_at: now,
            last_updated: now,
            days_collected: 0,
            unlock_distribution: HourlyDistribution::new(),
            interaction_distribution: HourlyDistribution::new(),
            avg_daily_unlocks: 0.0,
            avg_daily_interactions: 0.0,
            avg_session_duration_secs: 0.0,
            charge_distribution: HourlyDistribution::new(),
            location_clusters: Vec::new(),
        }
    }

    /// Add a day's behavioral observations to the profile.
    ///
    /// This is called once per day after the PoL attestation is finalized.
    /// The raw `DailyBehavioralData` is discarded after this call.
    pub fn update_profile(&mut self, data: &DailyBehavioralData) {
        self.days_collected += 1;
        self.last_updated = Utc::now();

        // --- Unlock patterns ---
        self.unlock_distribution.add_observations(&data.unlock_hours);

        // Running average of daily unlock count
        let n = self.days_collected as f64;
        self.avg_daily_unlocks =
            self.avg_daily_unlocks * ((n - 1.0) / n) + data.unlock_count as f64 / n;

        // --- Interaction patterns ---
        let interaction_hours: Vec<u8> = data.interaction_sessions.iter().map(|(h, _)| *h).collect();
        self.interaction_distribution.add_observations(&interaction_hours);

        self.avg_daily_interactions =
            self.avg_daily_interactions * ((n - 1.0) / n) + data.interaction_count as f64 / n;

        // Update average session duration
        if !data.interaction_sessions.is_empty() {
            let day_avg_duration: f64 = data
                .interaction_sessions
                .iter()
                .map(|(_, d)| *d as f64)
                .sum::<f64>()
                / data.interaction_sessions.len() as f64;
            self.avg_session_duration_secs =
                self.avg_session_duration_secs * ((n - 1.0) / n) + day_avg_duration / n;
        }

        // --- Charging patterns ---
        self.charge_distribution.add_observations(&data.charge_hours);

        // --- Location patterns ---
        for loc in &data.locations_visited {
            self.add_location_observation(loc.lat, loc.lon);
        }
    }

    /// Merge a GPS observation into the nearest cluster or create a new one.
    fn add_location_observation(&mut self, lat: f32, lon: f32) {
        // Find the nearest existing cluster
        let nearest = self
            .location_clusters
            .iter_mut()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = a.distance_km(lat, lon);
                let db = b.distance_km(lat, lon);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });

        match nearest {
            Some((_, cluster)) if cluster.distance_km(lat, lon) < CLUSTER_MERGE_RADIUS_KM => {
                cluster.add_observation(lat, lon);
            }
            _ => {
                if self.location_clusters.len() < MAX_LOCATION_CLUSTERS {
                    self.location_clusters.push(LocationCluster {
                        lat,
                        lon,
                        observation_count: 1,
                    });
                } else {
                    // WHY: If we are at the cluster limit, replace the least-visited
                    // cluster. This allows the profile to adapt over time as habits
                    // change, while keeping the most significant locations.
                    if let Some((min_idx, _)) = self
                        .location_clusters
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, c)| c.observation_count)
                    {
                        self.location_clusters[min_idx] = LocationCluster {
                            lat,
                            lon,
                            observation_count: 1,
                        };
                    }
                }
            }
        }
    }

    /// Compare this profile against another and return a similarity score.
    ///
    /// Returns a value from 0.0 (completely different human) to 1.0 (same human).
    /// Used during wallet recovery: the new device builds a fresh profile over
    /// 7-14 days, then we compare it against the old device's stored profile.
    ///
    /// The comparison weights multiple behavioral dimensions:
    /// - Unlock timing: 25% — when you use your phone is highly individual
    /// - Interaction timing: 20% — session patterns reflect personal habits
    /// - Activity level: 15% — how much you use your phone per day
    /// - Charging habits: 15% — charging time is surprisingly consistent
    /// - Location patterns: 25% — where you go is the strongest signal
    pub fn compare_profiles(&self, other: &BehavioralProfile) -> f64 {
        // WHY: We weight dimensions by their discriminative power, determined
        // by research on smartphone behavioral biometrics. Location and unlock
        // timing are the most individually distinctive patterns.
        const WEIGHT_UNLOCK_TIMING: f64 = 0.25;
        const WEIGHT_INTERACTION_TIMING: f64 = 0.20;
        const WEIGHT_ACTIVITY_LEVEL: f64 = 0.15;
        const WEIGHT_CHARGING: f64 = 0.15;
        const WEIGHT_LOCATION: f64 = 0.25;

        let mut total_score = 0.0_f64;

        // 1. Unlock timing similarity
        let unlock_sim = self
            .unlock_distribution
            .cosine_similarity(&other.unlock_distribution);
        total_score += unlock_sim * WEIGHT_UNLOCK_TIMING;

        // 2. Interaction timing similarity
        let interaction_sim = self
            .interaction_distribution
            .cosine_similarity(&other.interaction_distribution);
        total_score += interaction_sim * WEIGHT_INTERACTION_TIMING;

        // 3. Activity level similarity (daily unlocks + interactions + session duration)
        let activity_sim = Self::activity_level_similarity(
            self.avg_daily_unlocks,
            other.avg_daily_unlocks,
            self.avg_daily_interactions,
            other.avg_daily_interactions,
            self.avg_session_duration_secs,
            other.avg_session_duration_secs,
        );
        total_score += activity_sim * WEIGHT_ACTIVITY_LEVEL;

        // 4. Charging habit similarity
        let charge_sim = self
            .charge_distribution
            .cosine_similarity(&other.charge_distribution);
        total_score += charge_sim * WEIGHT_CHARGING;

        // 5. Location pattern similarity
        let location_sim = self.location_similarity(other);
        total_score += location_sim * WEIGHT_LOCATION;

        total_score.min(1.0).max(0.0)
    }

    /// Compare activity levels between two profiles.
    /// Uses a ratio-based similarity that is tolerant of natural variation.
    fn activity_level_similarity(
        unlocks_a: f64,
        unlocks_b: f64,
        interactions_a: f64,
        interactions_b: f64,
        duration_a: f64,
        duration_b: f64,
    ) -> f64 {
        let unlock_sim = Self::ratio_similarity(unlocks_a, unlocks_b);
        let interaction_sim = Self::ratio_similarity(interactions_a, interactions_b);
        let duration_sim = Self::ratio_similarity(duration_a, duration_b);
        (unlock_sim + interaction_sim + duration_sim) / 3.0
    }

    /// Compute similarity of two positive values as min/max ratio.
    /// Returns 1.0 if identical, approaches 0.0 as they diverge.
    /// WHY: Ratio-based comparison is scale-invariant and handles the case
    /// where one value is 0 gracefully.
    fn ratio_similarity(a: f64, b: f64) -> f64 {
        if a < 1e-9 && b < 1e-9 {
            return 1.0; // Both effectively zero
        }
        if a < 1e-9 || b < 1e-9 {
            return 0.0; // One is zero, the other isn't
        }
        a.min(b) / a.max(b)
    }

    /// Compare location patterns between two profiles.
    ///
    /// For each cluster in one profile, find the nearest cluster in the other
    /// and compute a match score. This is tolerant of the user visiting
    /// slightly different spots (e.g., different coffee shops) and handles
    /// the case where the new device may not have visited all locations yet.
    fn location_similarity(&self, other: &BehavioralProfile) -> f64 {
        if self.location_clusters.is_empty() || other.location_clusters.is_empty() {
            // WHY: If either profile has no location data, we cannot compare.
            // Return a neutral score rather than 0 to avoid penalizing users
            // who had GPS issues during the recovery period.
            return 0.5;
        }

        // For each cluster in `self`, find the closest cluster in `other`
        // and score based on distance.
        let mut match_scores: Vec<f64> = Vec::new();

        for cluster_a in &self.location_clusters {
            let best_match = other
                .location_clusters
                .iter()
                .map(|cluster_b| cluster_b.distance_km(cluster_a.lat, cluster_a.lon))
                .fold(f64::MAX, f64::min);

            // WHY: Score decays with distance. Within 2km = perfect match,
            // 2-10km = partial match, >10km = no match. These thresholds
            // reflect typical urban geography.
            let score = if best_match < CLUSTER_MERGE_RADIUS_KM {
                1.0
            } else if best_match < 10.0 {
                // Linear decay from 1.0 at 2km to 0.0 at 10km
                1.0 - (best_match - CLUSTER_MERGE_RADIUS_KM) / (10.0 - CLUSTER_MERGE_RADIUS_KM)
            } else {
                0.0
            };

            // Weight by observation count — frequently visited locations matter more.
            match_scores.push(score * cluster_a.observation_count as f64);
        }

        let total_weight: f64 = self
            .location_clusters
            .iter()
            .map(|c| c.observation_count as f64)
            .sum();

        if total_weight < 1e-9 {
            return 0.5;
        }

        let weighted_score: f64 = match_scores.iter().sum::<f64>() / total_weight;
        weighted_score.min(1.0)
    }

    /// Check whether this profile has enough data for a meaningful comparison.
    pub fn is_mature(&self) -> bool {
        self.days_collected >= MIN_PROFILE_DAYS
    }

    /// Check whether the recovery window has expired.
    pub fn recovery_window_expired(&self) -> bool {
        self.days_collected > MAX_RECOVERY_DAYS
    }

    /// Get the default similarity threshold for wallet recovery.
    pub fn default_match_threshold() -> f64 {
        DEFAULT_MATCH_THRESHOLD
    }

    /// Get the number of days collected.
    pub fn days_collected(&self) -> u32 {
        self.days_collected
    }
}

impl Default for BehavioralProfile {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a `DailyBehavioralData` from raw event data.
///
/// This is a convenience function that bridges the `SensorEventBuffer`
/// output to the behavioral profile input.
pub fn build_daily_behavioral_data(
    unlock_timestamps: &[DateTime<Utc>],
    interaction_sessions: &[(DateTime<Utc>, u32)],
    locations: &[GeoLocation],
    charge_timestamps: &[DateTime<Utc>],
) -> DailyBehavioralData {
    DailyBehavioralData {
        unlock_hours: unlock_timestamps.iter().map(|t| t.hour() as u8).collect(),
        interaction_sessions: interaction_sessions
            .iter()
            .map(|(t, d)| (t.hour() as u8, *d))
            .collect(),
        locations_visited: locations.to_vec(),
        charge_hours: charge_timestamps.iter().map(|t| t.hour() as u8).collect(),
        unlock_count: unlock_timestamps.len() as u32,
        interaction_count: interaction_sessions.len() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// Build a typical day's behavioral data for a "morning person" profile.
    fn morning_person_day() -> DailyBehavioralData {
        DailyBehavioralData {
            // Heavy usage 6am-12pm, light usage afternoon, minimal at night
            unlock_hours: vec![6, 6, 7, 7, 8, 8, 8, 9, 10, 11, 12, 14, 17, 21],
            interaction_sessions: vec![
                (6, 180), (7, 300), (8, 600), (9, 120), (10, 240),
                (12, 180), (14, 60), (17, 120), (21, 300),
            ],
            locations_visited: vec![
                GeoLocation { lat: 40.712, lon: -74.006 }, // Home
                GeoLocation { lat: 40.758, lon: -73.986 }, // Work
            ],
            charge_hours: vec![22, 7],
            unlock_count: 14,
            interaction_count: 9,
        }
    }

    /// Build a typical day's behavioral data for a "night owl" profile.
    fn night_owl_day() -> DailyBehavioralData {
        DailyBehavioralData {
            // Light usage morning, heavy usage evening/night
            unlock_hours: vec![10, 11, 13, 15, 17, 18, 19, 19, 20, 21, 22, 22, 23, 23, 0, 1],
            interaction_sessions: vec![
                (10, 60), (13, 120), (17, 180), (19, 600), (20, 300),
                (21, 480), (22, 360), (23, 240), (0, 120),
            ],
            locations_visited: vec![
                GeoLocation { lat: 34.052, lon: -118.244 }, // Home (LA)
                GeoLocation { lat: 34.019, lon: -118.411 }, // Work (LA)
            ],
            charge_hours: vec![3, 15],
            unlock_count: 16,
            interaction_count: 9,
        }
    }

    #[test]
    fn test_new_profile_is_empty() {
        let profile = BehavioralProfile::new();
        assert_eq!(profile.days_collected, 0);
        assert!(!profile.is_mature());
    }

    #[test]
    fn test_profile_matures_after_min_days() {
        let mut profile = BehavioralProfile::new();
        let data = morning_person_day();
        for _ in 0..MIN_PROFILE_DAYS {
            profile.update_profile(&data);
        }
        assert!(profile.is_mature());
        assert_eq!(profile.days_collected, MIN_PROFILE_DAYS);
    }

    #[test]
    fn test_recovery_window_expiry() {
        let mut profile = BehavioralProfile::new();
        let data = morning_person_day();
        for _ in 0..=MAX_RECOVERY_DAYS {
            profile.update_profile(&data);
        }
        assert!(profile.recovery_window_expired());
    }

    #[test]
    fn test_same_person_high_similarity() {
        let mut profile_a = BehavioralProfile::new();
        let mut profile_b = BehavioralProfile::new();
        let data = morning_person_day();

        // Both profiles built from the same behavioral pattern
        for _ in 0..10 {
            profile_a.update_profile(&data);
            profile_b.update_profile(&data);
        }

        let similarity = profile_a.compare_profiles(&profile_b);
        assert!(
            similarity > 0.85,
            "Same-person similarity should be high, got {similarity}"
        );
    }

    #[test]
    fn test_different_people_low_similarity() {
        let mut profile_morning = BehavioralProfile::new();
        let mut profile_night = BehavioralProfile::new();

        for _ in 0..10 {
            profile_morning.update_profile(&morning_person_day());
            profile_night.update_profile(&night_owl_day());
        }

        let similarity = profile_morning.compare_profiles(&profile_night);
        assert!(
            similarity < DEFAULT_MATCH_THRESHOLD,
            "Different-person similarity should be below threshold, got {similarity}"
        );
    }

    #[test]
    fn test_empty_profiles_comparison() {
        let profile_a = BehavioralProfile::new();
        let profile_b = BehavioralProfile::new();
        let similarity = profile_a.compare_profiles(&profile_b);
        // Empty profiles should produce a neutral/low score
        assert!(similarity <= 0.5);
    }

    #[test]
    fn test_hourly_distribution_cosine_similarity() {
        let mut dist_a = HourlyDistribution::new();
        let mut dist_b = HourlyDistribution::new();

        // Identical distributions
        let hours: Vec<u8> = vec![8, 8, 9, 10, 12, 14, 17, 21];
        dist_a.add_observations(&hours);
        dist_b.add_observations(&hours);

        let sim = dist_a.cosine_similarity(&dist_b);
        assert!(
            (sim - 1.0).abs() < 0.01,
            "Identical distributions should have ~1.0 similarity, got {sim}"
        );
    }

    #[test]
    fn test_hourly_distribution_opposite_patterns() {
        let mut dist_morning = HourlyDistribution::new();
        let mut dist_night = HourlyDistribution::new();

        // Morning person: all activity 6-10am
        dist_morning.add_observations(&[6, 7, 7, 8, 8, 9, 10]);
        // Night owl: all activity 10pm-2am
        dist_night.add_observations(&[22, 22, 23, 23, 0, 0, 1]);

        let sim = dist_morning.cosine_similarity(&dist_night);
        assert!(
            sim < 0.1,
            "Opposite patterns should have very low similarity, got {sim}"
        );
    }

    #[test]
    fn test_ratio_similarity() {
        assert!((BehavioralProfile::ratio_similarity(10.0, 10.0) - 1.0).abs() < 1e-9);
        assert!((BehavioralProfile::ratio_similarity(10.0, 20.0) - 0.5).abs() < 1e-9);
        assert!((BehavioralProfile::ratio_similarity(0.0, 0.0) - 1.0).abs() < 1e-9);
        assert!((BehavioralProfile::ratio_similarity(0.0, 10.0)).abs() < 1e-9);
    }

    #[test]
    fn test_location_cluster_merging() {
        let mut profile = BehavioralProfile::new();

        // Add multiple observations near the same point
        let data1 = DailyBehavioralData {
            unlock_hours: vec![8],
            interaction_sessions: vec![(8, 60)],
            locations_visited: vec![
                GeoLocation { lat: 40.712, lon: -74.006 },
                GeoLocation { lat: 40.713, lon: -74.005 }, // ~150m away — same cluster
            ],
            charge_hours: vec![22],
            unlock_count: 1,
            interaction_count: 1,
        };
        profile.update_profile(&data1);

        // Should merge into one cluster
        assert_eq!(profile.location_clusters.len(), 1);
        assert_eq!(profile.location_clusters[0].observation_count, 2);
    }

    #[test]
    fn test_location_clusters_separate_distinct_locations() {
        let mut profile = BehavioralProfile::new();

        let data = DailyBehavioralData {
            unlock_hours: vec![8],
            interaction_sessions: vec![(8, 60)],
            locations_visited: vec![
                GeoLocation { lat: 40.712, lon: -74.006 }, // NYC
                GeoLocation { lat: 34.052, lon: -118.244 }, // LA — very far
            ],
            charge_hours: vec![22],
            unlock_count: 1,
            interaction_count: 1,
        };
        profile.update_profile(&data);

        // Should create two separate clusters
        assert_eq!(profile.location_clusters.len(), 2);
    }

    #[test]
    fn test_profile_update_increments_days() {
        let mut profile = BehavioralProfile::new();
        let data = morning_person_day();
        profile.update_profile(&data);
        assert_eq!(profile.days_collected, 1);
        profile.update_profile(&data);
        assert_eq!(profile.days_collected, 2);
    }

    #[test]
    fn test_build_daily_behavioral_data() {
        let now = Utc::now();
        let unlocks = vec![
            now - Duration::hours(8),
            now - Duration::hours(5),
            now - Duration::hours(1),
        ];
        let interactions = vec![(now - Duration::hours(7), 120u32), (now - Duration::hours(3), 60)];
        let locations = vec![GeoLocation { lat: 51.5, lon: -0.1 }];
        let charges = vec![now - Duration::hours(10)];

        let data = build_daily_behavioral_data(&unlocks, &interactions, &locations, &charges);
        assert_eq!(data.unlock_count, 3);
        assert_eq!(data.interaction_count, 2);
        assert_eq!(data.locations_visited.len(), 1);
        assert_eq!(data.charge_hours.len(), 1);
    }

    #[test]
    fn test_similarity_above_threshold_for_recovery() {
        // Simulate a real recovery scenario: same person, slightly different
        // data due to new device / different circumstances.
        let mut original = BehavioralProfile::new();
        let mut recovery = BehavioralProfile::new();

        // Build original profile over 30 days
        for _ in 0..30 {
            original.update_profile(&morning_person_day());
        }

        // Build recovery profile over 10 days with slight variations
        for i in 0..10 {
            let mut day = morning_person_day();
            // Slight variation: sometimes an extra unlock, sometimes one less
            if i % 3 == 0 {
                day.unlock_hours.push(15);
                day.unlock_count += 1;
            }
            if i % 4 == 0 {
                day.interaction_sessions.push((16, 90));
                day.interaction_count += 1;
            }
            recovery.update_profile(&day);
        }

        let similarity = original.compare_profiles(&recovery);
        assert!(
            similarity >= DEFAULT_MATCH_THRESHOLD,
            "Recovery profile should match original above threshold, got {similarity}"
        );
    }

    #[test]
    fn test_impostor_below_threshold() {
        let mut original = BehavioralProfile::new();
        let mut impostor = BehavioralProfile::new();

        // Original is a morning person in NYC
        for _ in 0..30 {
            original.update_profile(&morning_person_day());
        }

        // Impostor is a night owl in LA
        for _ in 0..10 {
            impostor.update_profile(&night_owl_day());
        }

        let similarity = original.compare_profiles(&impostor);
        assert!(
            similarity < DEFAULT_MATCH_THRESHOLD,
            "Impostor should be below threshold, got {similarity}"
        );
    }
}
