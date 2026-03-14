//! On-chain polling system.
//!
//! Any GRAT holder can create a poll. Every response comes from a
//! Proof-of-Life-verified unique human. One phone, one vote per poll.
//! Results are on-chain, publicly auditable, tamper-proof.
//!
//! Use cases: protocol governance, political polling, market research,
//! community decisions, dispute resolution.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use gratia_core::config::GovernanceConfig;
use gratia_core::types::{Address, GeographicFilter, GeoLocation, Lux, NodeId, Poll};

use crate::error::GovernanceError;

/// Record of a single poll vote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollVoteRecord {
    pub voter: NodeId,
    pub option_index: u32,
    pub timestamp: DateTime<Utc>,
}

/// Manages all on-chain polls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollStore {
    polls: HashMap<[u8; 32], Poll>,
    /// Tracks which nodes have voted on which polls.
    voted: HashMap<[u8; 32], HashSet<NodeId>>,
    /// Full vote records per poll.
    records: HashMap<[u8; 32], Vec<PollVoteRecord>>,
    /// Counter for ID generation.
    next_poll_nonce: u64,
}

impl PollStore {
    pub fn new() -> Self {
        Self {
            polls: HashMap::new(),
            voted: HashMap::new(),
            records: HashMap::new(),
            next_poll_nonce: 0,
        }
    }

    /// Create a new on-chain poll.
    ///
    /// The caller is responsible for verifying the creator holds enough GRAT
    /// and burning the creation fee. `creator_balance` is checked here for
    /// a clear error message but the actual deduction happens at the transaction layer.
    pub fn create_poll(
        &mut self,
        creator: Address,
        question: String,
        options: Vec<String>,
        duration_secs: u64,
        geographic_filter: Option<GeographicFilter>,
        creator_balance: Lux,
        config: &GovernanceConfig,
        now: DateTime<Utc>,
    ) -> Result<[u8; 32], GovernanceError> {
        if question.is_empty() {
            return Err(GovernanceError::EmptyQuestion);
        }

        if options.len() < 2 {
            return Err(GovernanceError::TooFewOptions);
        }

        if creator_balance < config.poll_creation_fee {
            return Err(GovernanceError::InsufficientBalance {
                available: creator_balance,
                required: config.poll_creation_fee,
            });
        }

        let id = self.generate_id(&creator, now);
        let expires_at = now + Duration::seconds(duration_secs as i64);

        let vote_counts = vec![0u64; options.len()];

        let poll = Poll {
            id,
            creator,
            question,
            options,
            created_at: now,
            expires_at,
            votes: vote_counts,
            total_voters: 0,
            geographic_filter,
            creation_fee: config.poll_creation_fee,
        };

        self.polls.insert(id, poll);

        tracing::info!(
            poll_id = hex::encode(id),
            creator = %creator,
            "on-chain poll created"
        );

        Ok(id)
    }

    /// Cast a vote on a poll.
    ///
    /// Requirements:
    /// - Poll must not have expired.
    /// - Voter must have valid PoL (caller verifies, passes `has_valid_pol`).
    /// - Voter must not have already voted.
    /// - If the poll has a geographic filter, voter's location must be within range.
    pub fn cast_poll_vote(
        &mut self,
        poll_id: &[u8; 32],
        voter: NodeId,
        option_index: u32,
        has_valid_pol: bool,
        voter_location: Option<GeoLocation>,
        now: DateTime<Utc>,
    ) -> Result<(), GovernanceError> {
        if !has_valid_pol {
            return Err(GovernanceError::NoValidProofOfLife);
        }

        let poll = self.polls.get(poll_id).ok_or_else(|| {
            GovernanceError::PollNotFound {
                id: hex::encode(poll_id),
            }
        })?;

        if now >= poll.expires_at {
            return Err(GovernanceError::PollExpired);
        }

        if option_index as usize >= poll.options.len() {
            return Err(GovernanceError::InvalidOptionIndex {
                index: option_index,
                count: poll.options.len(),
            });
        }

        // Check geographic filter if present.
        if let Some(ref filter) = poll.geographic_filter {
            let location = voter_location.ok_or(GovernanceError::OutsideGeographicFilter)?;
            if !is_within_radius(location, filter) {
                return Err(GovernanceError::OutsideGeographicFilter);
            }
        }

        // Prevent double voting.
        let voters = self.voted.entry(*poll_id).or_default();
        if voters.contains(&voter) {
            return Err(GovernanceError::AlreadyVotedPoll { node_id: voter });
        }

        voters.insert(voter);
        self.records
            .entry(*poll_id)
            .or_default()
            .push(PollVoteRecord {
                voter,
                option_index,
                timestamp: now,
            });

        // Update poll tally.
        let poll = self.polls.get_mut(poll_id).unwrap();
        poll.votes[option_index as usize] += 1;
        poll.total_voters += 1;

        Ok(())
    }

    /// Get poll results. Publicly auditable.
    pub fn get_poll_results(&self, poll_id: &[u8; 32]) -> Option<PollResults> {
        let poll = self.polls.get(poll_id)?;

        let option_results: Vec<PollOptionResult> = poll
            .options
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let votes = poll.votes[i];
                let percentage = if poll.total_voters > 0 {
                    (votes as f64 / poll.total_voters as f64) * 100.0
                } else {
                    0.0
                };
                PollOptionResult {
                    index: i as u32,
                    label: label.clone(),
                    votes,
                    percentage,
                }
            })
            .collect();

        Some(PollResults {
            poll_id: poll.id,
            question: poll.question.clone(),
            total_voters: poll.total_voters,
            options: option_results,
            expired: Utc::now() >= poll.expires_at,
        })
    }

    // -- Accessors --

    pub fn get_poll(&self, id: &[u8; 32]) -> Option<&Poll> {
        self.polls.get(id)
    }

    /// Return all polls that have not yet expired.
    pub fn active_polls(&self, now: DateTime<Utc>) -> Vec<&Poll> {
        self.polls.values().filter(|p| now < p.expires_at).collect()
    }

    /// Return all polls.
    pub fn all_polls(&self) -> Vec<&Poll> {
        self.polls.values().collect()
    }

    /// Get vote records for a poll (publicly auditable).
    pub fn get_records(&self, poll_id: &[u8; 32]) -> &[PollVoteRecord] {
        self.records
            .get(poll_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Check whether a specific node has voted on a poll.
    pub fn has_voted(&self, poll_id: &[u8; 32], node_id: &NodeId) -> bool {
        self.voted
            .get(poll_id)
            .map(|set| set.contains(node_id))
            .unwrap_or(false)
    }

    /// Generate a deterministic poll ID.
    fn generate_id(&mut self, creator: &Address, now: DateTime<Utc>) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-poll-v1:");
        hasher.update(creator.0);
        hasher.update(now.timestamp().to_le_bytes());
        hasher.update(self.next_poll_nonce.to_le_bytes());
        self.next_poll_nonce += 1;

        let result = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&result);
        id
    }
}

impl Default for PollStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Aggregated results for a poll.
#[derive(Debug, Clone)]
pub struct PollResults {
    pub poll_id: [u8; 32],
    pub question: String,
    pub total_voters: u64,
    pub options: Vec<PollOptionResult>,
    pub expired: bool,
}

/// Result for a single poll option.
#[derive(Debug, Clone)]
pub struct PollOptionResult {
    pub index: u32,
    pub label: String,
    pub votes: u64,
    pub percentage: f64,
}

/// Check whether a location falls within the geographic filter radius.
///
/// Uses the Haversine formula for great-circle distance on a sphere.
fn is_within_radius(location: GeoLocation, filter: &GeographicFilter) -> bool {
    // WHY: Haversine gives sufficient accuracy for poll geographic filtering
    // at city/region scale. No need for Vincenty or more expensive calculations.
    const EARTH_RADIUS_KM: f64 = 6371.0;

    let lat1 = (location.lat as f64).to_radians();
    let lat2 = filter.lat.to_radians();
    let dlat = (filter.lat - location.lat as f64).to_radians();
    let dlon = (filter.lon - location.lon as f64).to_radians();

    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    let distance_km = EARTH_RADIUS_KM * c;

    distance_km <= filter.radius_km
}

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::config::GovernanceConfig;
    use gratia_core::types::LUX_PER_GRAT;

    fn test_node(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId(bytes)
    }

    fn test_address(id: u8) -> Address {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        Address(bytes)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn default_config() -> GovernanceConfig {
        GovernanceConfig::default()
    }

    #[test]
    fn test_create_poll() {
        let mut store = PollStore::new();
        let config = default_config();
        let creator = test_address(1);
        // 100 GRAT balance, fee is 10 GRAT.
        let balance = 100 * LUX_PER_GRAT;

        let id = store
            .create_poll(
                creator,
                "Do you approve?".into(),
                vec!["Yes".into(), "No".into()],
                86400, // 1 day
                None,
                balance,
                &config,
                now(),
            )
            .unwrap();

        let poll = store.get_poll(&id).unwrap();
        assert_eq!(poll.options.len(), 2);
        assert_eq!(poll.total_voters, 0);
    }

    #[test]
    fn test_create_poll_insufficient_balance() {
        let mut store = PollStore::new();
        let config = default_config();

        let result = store.create_poll(
            test_address(1),
            "Question".into(),
            vec!["A".into(), "B".into()],
            86400,
            None,
            1, // 1 Lux, way below fee.
            &config,
            now(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_create_poll_too_few_options() {
        let mut store = PollStore::new();
        let config = default_config();

        let result = store.create_poll(
            test_address(1),
            "Question".into(),
            vec!["Only one".into()],
            86400,
            None,
            100 * LUX_PER_GRAT,
            &config,
            now(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_cast_poll_vote_success() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .create_poll(
                test_address(1),
                "Best color?".into(),
                vec!["Red".into(), "Blue".into(), "Green".into()],
                86400,
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        store
            .cast_poll_vote(&id, test_node(10), 1, true, None, ts)
            .unwrap();

        let poll = store.get_poll(&id).unwrap();
        assert_eq!(poll.votes[1], 1);
        assert_eq!(poll.total_voters, 1);
        assert!(store.has_voted(&id, &test_node(10)));
    }

    #[test]
    fn test_double_poll_vote_rejected() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .create_poll(
                test_address(1),
                "Q".into(),
                vec!["A".into(), "B".into()],
                86400,
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        store
            .cast_poll_vote(&id, test_node(10), 0, true, None, ts)
            .unwrap();

        let result = store.cast_poll_vote(&id, test_node(10), 1, true, None, ts);
        assert!(result.is_err());
    }

    #[test]
    fn test_expired_poll_vote_rejected() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .create_poll(
                test_address(1),
                "Q".into(),
                vec!["A".into(), "B".into()],
                60, // 60 seconds
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        // Vote after expiry.
        let after = ts + Duration::seconds(61);
        let result = store.cast_poll_vote(&id, test_node(10), 0, true, None, after);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_option_index() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .create_poll(
                test_address(1),
                "Q".into(),
                vec!["A".into(), "B".into()],
                86400,
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        let result = store.cast_poll_vote(&id, test_node(10), 5, true, None, ts);
        assert!(result.is_err());
    }

    #[test]
    fn test_geographic_filter() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        // Poll centered on New York City, 50km radius.
        let filter = GeographicFilter {
            lat: 40.7128,
            lon: -74.0060,
            radius_km: 50.0,
        };

        let id = store
            .create_poll(
                test_address(1),
                "NYC question".into(),
                vec!["A".into(), "B".into()],
                86400,
                Some(filter),
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        // Voter in NYC — should succeed.
        let nyc_location = GeoLocation {
            lat: 40.73,
            lon: -73.99,
        };
        store
            .cast_poll_vote(&id, test_node(10), 0, true, Some(nyc_location), ts)
            .unwrap();

        // Voter in London — should fail.
        let london_location = GeoLocation {
            lat: 51.5074,
            lon: -0.1278,
        };
        let result = store.cast_poll_vote(&id, test_node(11), 0, true, Some(london_location), ts);
        assert!(result.is_err());
    }

    #[test]
    fn test_no_location_for_filtered_poll_rejected() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        let filter = GeographicFilter {
            lat: 0.0,
            lon: 0.0,
            radius_km: 100.0,
        };

        let id = store
            .create_poll(
                test_address(1),
                "Q".into(),
                vec!["A".into(), "B".into()],
                86400,
                Some(filter),
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        // No location provided for a geo-filtered poll.
        let result = store.cast_poll_vote(&id, test_node(10), 0, true, None, ts);
        assert!(result.is_err());
    }

    #[test]
    fn test_poll_results() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .create_poll(
                test_address(1),
                "Favorite?".into(),
                vec!["Alpha".into(), "Beta".into(), "Gamma".into()],
                86400,
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        store.cast_poll_vote(&id, test_node(1), 0, true, None, ts).unwrap();
        store.cast_poll_vote(&id, test_node(2), 0, true, None, ts).unwrap();
        store.cast_poll_vote(&id, test_node(3), 1, true, None, ts).unwrap();

        let results = store.get_poll_results(&id).unwrap();
        assert_eq!(results.total_voters, 3);
        assert_eq!(results.options[0].votes, 2);
        assert_eq!(results.options[1].votes, 1);
        assert_eq!(results.options[2].votes, 0);
    }

    #[test]
    fn test_active_polls() {
        let mut store = PollStore::new();
        let config = default_config();
        let ts = now();

        // Create two polls: one short (60s) and one long (86400s).
        store
            .create_poll(
                test_address(1),
                "Short".into(),
                vec!["A".into(), "B".into()],
                60,
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        store
            .create_poll(
                test_address(2),
                "Long".into(),
                vec!["A".into(), "B".into()],
                86400,
                None,
                100 * LUX_PER_GRAT,
                &config,
                ts,
            )
            .unwrap();

        // At ts + 120s, the short poll should be expired.
        let after = ts + Duration::seconds(120);
        let active = store.active_polls(after);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].question, "Long");
    }
}
