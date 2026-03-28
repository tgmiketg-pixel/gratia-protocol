//! Behavioral clustering detection — network-level defense against phone farms.
//!
//! Phone farms (co-located devices managed by a single operator) exhibit
//! detectable patterns even when each device individually passes Proof of Life:
//!
//! - **Shared Bluetooth peers:** Phones in the same physical location see the
//!   same nearby BT devices day after day.
//! - **Synchronized mining:** Farm phones are plugged in and start mining at
//!   the same time because one person manages them all.
//! - **Behavioral correlation:** Daily rhythms (charge times, unlock patterns)
//!   are suspiciously similar across devices that should be independent humans.
//!
//! PRIVACY: Nodes submit a SHA-256 hash of their daily Bluetooth peer set, NOT
//! the actual device IDs. The network compares hashes across nodes — matching
//! hashes indicate shared physical environment without revealing which specific
//! devices were detected. No individual device identity is ever transmitted.
//!
//! This module runs on validators/beacon chain nodes, not on individual phones.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::crypto::sha256;
use gratia_core::types::NodeId;

// ============================================================================
// Peer Set Hash
// ============================================================================

/// A privacy-preserving hash of a node's daily Bluetooth peer set.
///
/// WHY: Nodes report a hash of their BT peers seen that day, not the actual
/// device IDs. The network can compare hashes across nodes without knowing
/// which specific devices were detected. Two nodes in the same room will
/// hash the same peer set and produce identical `PeerSetHash` values, which
/// is the signal we detect — but neither the network nor other nodes ever
/// learn which Bluetooth devices were present.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerSetHash(pub [u8; 32]);

impl PeerSetHash {
    /// Create a `PeerSetHash` from a set of Bluetooth peer MAC addresses.
    ///
    /// WHY: Sorting before hashing ensures that the same set of peers always
    /// produces the same hash regardless of discovery order. This is critical
    /// because BT discovery order is non-deterministic — without sorting,
    /// two phones in the same room could produce different hashes for the
    /// same peer set.
    pub fn from_peer_ids(peer_ids: &[[u8; 6]]) -> Self {
        let mut sorted = peer_ids.to_vec();
        sorted.sort();

        // Concatenate all 6-byte MAC addresses into a single byte buffer.
        let mut data = Vec::with_capacity(sorted.len() * 6);
        for peer in &sorted {
            data.extend_from_slice(peer);
        }

        PeerSetHash(sha256(&data))
    }
}

// ============================================================================
// Node Daily Report
// ============================================================================

/// Data submitted by each node for cluster analysis.
///
/// This is a lightweight summary — no raw sensor data, no identifiable
/// information beyond the node's own ID. Submitted alongside the daily
/// Proof of Life attestation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDailyReport {
    /// The node submitting this report.
    pub node_id: NodeId,
    /// UTC date/time of this report.
    pub date: DateTime<Utc>,
    /// Privacy-preserving hash of the BT peers seen today.
    pub peer_set_hash: PeerSetHash,
    /// Hour of day when charging started (0.0-24.0).
    pub charge_start_hour: f32,
    /// Hour of day when charging ended (0.0-24.0).
    pub charge_end_hour: f32,
    /// Hour of day when mining started (0.0-24.0).
    pub mining_start_hour: f32,
    /// Total minutes of mining activity for the day.
    pub mining_duration_minutes: u32,
    /// Hash of daily unlock timing pattern.
    /// WHY: 16 bytes is sufficient for pattern comparison — we only need to
    /// detect whether two nodes have suspiciously similar unlock rhythms,
    /// not reconstruct the actual unlock times.
    pub unlock_pattern_hash: [u8; 16],
}

// ============================================================================
// Cluster Detection
// ============================================================================

/// Analyzes daily reports from multiple nodes to detect clusters of
/// potentially co-located or coordinated devices (phone farms).
///
/// The detector compares pairs of nodes across multiple dimensions and
/// flags clusters that exceed configurable thresholds.
pub struct ClusterDetector {
    /// Minimum days of shared peer hashes before flagging a cluster.
    /// WHY: A single day of matching peer hashes could be coincidence (e.g.,
    /// two users on the same bus). 14 days of consistent co-location is
    /// strong evidence of a farm.
    pub min_overlap_days: u32,

    /// Minimum peer set hash overlap fraction (0.0-1.0) to consider "same location".
    /// WHY: 0.8 allows for occasional days where one device is taken elsewhere
    /// (e.g., a farm operator's personal phone leaves the farm sometimes).
    /// Pure farms will be at 1.0; 0.8 catches farms with slight variation.
    pub overlap_threshold: f64,

    /// Time window (hours) for "simultaneous" mining start.
    /// WHY: 1 hour is generous enough to account for minor timing differences
    /// (phones reaching 80% battery at slightly different times) but tight
    /// enough to catch farms where all phones are plugged in together.
    pub mining_sync_window_hours: f32,
}

impl Default for ClusterDetector {
    fn default() -> Self {
        ClusterDetector {
            min_overlap_days: 14,
            overlap_threshold: 0.8,
            mining_sync_window_hours: 1.0,
        }
    }
}

/// A detected cluster of suspicious nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterAlert {
    /// The nodes identified in this cluster.
    pub node_ids: Vec<NodeId>,
    /// Number of days these nodes shared matching peer set hashes.
    pub shared_peer_days: u32,
    /// Average mining start time difference (hours) between nodes.
    pub avg_mining_sync: f32,
    /// Cluster confidence score (0.0-1.0).
    pub confidence: f64,
    /// Type of cluster detected.
    pub cluster_type: ClusterType,
}

/// Classification of detected cluster behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterType {
    /// Nodes consistently see the same Bluetooth peers (co-located).
    CoLocatedPeers,
    /// Nodes have synchronized mining patterns (managed farm).
    SynchronizedMining,
    /// Both co-located and synchronized — strong farm signature.
    FarmSignature,
}

impl ClusterDetector {
    /// Create a new `ClusterDetector` with default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Analyze reports from multiple nodes and return any detected clusters.
    ///
    /// `reports` is a slice of per-node report histories. Each inner `Vec`
    /// contains all daily reports for one node, ordered by date.
    ///
    /// WHY: This is O(n^2) pairwise comparison. This is acceptable because
    /// cluster detection runs on validators/beacon chain nodes (servers or
    /// high-end phones acting as validators), NOT on individual mining phones.
    /// For the PoC, n is small (hundreds of nodes). At mainnet scale, this
    /// would be batched by geographic shard so n stays manageable per shard.
    pub fn analyze_reports(&self, reports: &[Vec<NodeDailyReport>]) -> Vec<ClusterAlert> {
        let mut alerts = Vec::new();

        for i in 0..reports.len() {
            for j in (i + 1)..reports.len() {
                if let Some(alert) = self.check_pair(&reports[i], &reports[j]) {
                    alerts.push(alert);
                }
            }
        }

        alerts
    }

    /// Compare two nodes' report histories and return an alert if suspicious.
    ///
    /// Checks three dimensions:
    /// 1. Peer set hash overlap (co-location signal)
    /// 2. Mining start time synchronization (managed-farm signal)
    /// 3. Combined signal (farm signature)
    pub fn check_pair(
        &self,
        node_a: &[NodeDailyReport],
        node_b: &[NodeDailyReport],
    ) -> Option<ClusterAlert> {
        if node_a.is_empty() || node_b.is_empty() {
            return None;
        }

        let node_a_id = node_a[0].node_id;
        let node_b_id = node_b[0].node_id;

        // --- Dimension 1: Peer set hash overlap ---
        // Count days where both nodes reported the same peer set hash.
        let mut shared_peer_days: u32 = 0;
        let mut comparable_days: u32 = 0;

        for report_a in node_a {
            for report_b in node_b {
                // WHY: Compare dates at day granularity — same calendar day (UTC).
                if report_a.date.date_naive() == report_b.date.date_naive() {
                    comparable_days += 1;
                    if report_a.peer_set_hash == report_b.peer_set_hash {
                        shared_peer_days += 1;
                    }
                }
            }
        }

        let overlap_fraction = if comparable_days > 0 {
            shared_peer_days as f64 / comparable_days as f64
        } else {
            0.0
        };

        let is_colocated =
            shared_peer_days >= self.min_overlap_days && overlap_fraction >= self.overlap_threshold;

        // --- Dimension 2: Mining start time synchronization ---
        let mut mining_diffs = Vec::new();

        for report_a in node_a {
            for report_b in node_b {
                if report_a.date.date_naive() == report_b.date.date_naive() {
                    let diff = (report_a.mining_start_hour - report_b.mining_start_hour).abs();
                    mining_diffs.push(diff);
                }
            }
        }

        let avg_mining_sync = if mining_diffs.is_empty() {
            24.0 // No comparable data — maximum difference
        } else {
            mining_diffs.iter().sum::<f32>() / mining_diffs.len() as f32
        };

        let is_synchronized = avg_mining_sync < self.mining_sync_window_hours;

        // --- Determine cluster type and confidence ---
        let cluster_type = match (is_colocated, is_synchronized) {
            (true, true) => ClusterType::FarmSignature,
            (true, false) => ClusterType::CoLocatedPeers,
            (false, true) => ClusterType::SynchronizedMining,
            (false, false) => return None,
        };

        // WHY: Confidence formula combines two independent signals:
        // - shared_peer_days / 30.0: More days of co-location = higher confidence (caps at 1.0)
        // - (1.0 - avg_mining_diff / 24.0): Tighter mining synchronization = higher confidence
        // The product ensures both signals must be present for high confidence.
        let peer_confidence = (shared_peer_days as f64 / 30.0).min(1.0);
        let mining_confidence = (1.0 - avg_mining_sync as f64 / 24.0).max(0.0);
        let confidence = peer_confidence * mining_confidence;

        Some(ClusterAlert {
            node_ids: vec![node_a_id, node_b_id],
            shared_peer_days,
            avg_mining_sync,
            confidence,
            cluster_type,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Helper to build a `NodeDailyReport` with minimal boilerplate.
    fn make_report(
        node_byte: u8,
        day: u32,
        peer_hash: [u8; 32],
        charge_hour: f32,
        mining_hour: f32,
    ) -> NodeDailyReport {
        let mut node_id_bytes = [0u8; 32];
        node_id_bytes[0] = node_byte;

        NodeDailyReport {
            node_id: NodeId(node_id_bytes),
            date: Utc.with_ymd_and_hms(2026, 1, 1 + day, 12, 0, 0).unwrap(),
            peer_set_hash: PeerSetHash(peer_hash),
            charge_start_hour: charge_hour,
            charge_end_hour: charge_hour + 2.0,
            mining_start_hour: mining_hour,
            mining_duration_minutes: 120,
            unlock_pattern_hash: [node_byte; 16],
        }
    }

    #[test]
    fn test_peer_set_hash_deterministic() {
        let peers = [[1u8; 6], [2u8; 6], [3u8; 6]];
        let hash_a = PeerSetHash::from_peer_ids(&peers);
        let hash_b = PeerSetHash::from_peer_ids(&peers);
        assert_eq!(hash_a, hash_b, "Same peer set must produce identical hashes");
    }

    #[test]
    fn test_peer_set_hash_order_independent() {
        let peers_a = [[1u8; 6], [2u8; 6], [3u8; 6]];
        let peers_b = [[3u8; 6], [1u8; 6], [2u8; 6]];
        let hash_a = PeerSetHash::from_peer_ids(&peers_a);
        let hash_b = PeerSetHash::from_peer_ids(&peers_b);
        assert_eq!(hash_a, hash_b, "Peer order must not affect the hash");
    }

    #[test]
    fn test_no_cluster_with_different_peers() {
        let detector = ClusterDetector::default();

        // Two nodes with completely different peer hashes every day.
        let hash_a = [0xAAu8; 32];
        let hash_b = [0xBBu8; 32];

        let node_a: Vec<_> = (0..20).map(|d| make_report(1, d, hash_a, 22.0, 22.5)).collect();
        let node_b: Vec<_> = (0..20).map(|d| make_report(2, d, hash_b, 22.0, 22.5)).collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);

        // Should not flag CoLocatedPeers or FarmSignature since peer hashes differ.
        let has_colocation = alerts.iter().any(|a| {
            a.cluster_type == ClusterType::CoLocatedPeers
                || a.cluster_type == ClusterType::FarmSignature
        });
        assert!(!has_colocation, "Different peer hashes must not trigger co-location alert");
    }

    #[test]
    fn test_colocation_detected() {
        let detector = ClusterDetector::default();

        // Two nodes with the SAME peer hash for 20 days (well above 14-day threshold).
        let shared_hash = [0xCCu8; 32];

        // WHY: Mining hours differ by 6 hours so only the co-location signal fires,
        // not the synchronized mining signal.
        let node_a: Vec<_> = (0..20).map(|d| make_report(1, d, shared_hash, 8.0, 8.5)).collect();
        let node_b: Vec<_> = (0..20).map(|d| make_report(2, d, shared_hash, 8.0, 14.5)).collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);

        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].cluster_type, ClusterType::CoLocatedPeers);
        assert_eq!(alerts[0].shared_peer_days, 20);
    }

    #[test]
    fn test_synchronized_mining_detected() {
        let detector = ClusterDetector::default();

        // Two nodes with different peer hashes but near-identical mining start times.
        let hash_a = [0xAAu8; 32];
        let hash_b = [0xBBu8; 32];

        let node_a: Vec<_> = (0..20).map(|d| make_report(1, d, hash_a, 22.0, 22.0)).collect();
        let node_b: Vec<_> = (0..20).map(|d| make_report(2, d, hash_b, 22.0, 22.3)).collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);

        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].cluster_type, ClusterType::SynchronizedMining);
        assert!(
            alerts[0].avg_mining_sync < 1.0,
            "Average mining sync should be under 1 hour"
        );
    }

    #[test]
    fn test_farm_signature_both_signals() {
        let detector = ClusterDetector::default();

        // Two nodes: same peer hash AND synchronized mining — strongest farm signal.
        let shared_hash = [0xDDu8; 32];

        let node_a: Vec<_> =
            (0..20).map(|d| make_report(1, d, shared_hash, 22.0, 22.0)).collect();
        let node_b: Vec<_> =
            (0..20).map(|d| make_report(2, d, shared_hash, 22.0, 22.2)).collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);

        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].cluster_type, ClusterType::FarmSignature);
        assert!(
            alerts[0].confidence > 0.5,
            "Farm signature should have high confidence"
        );
    }

    #[test]
    fn test_below_threshold_no_alert() {
        let detector = ClusterDetector::default();

        // Only 5 days of overlap — below the 14-day threshold.
        let shared_hash = [0xEEu8; 32];

        let node_a: Vec<_> = (0..5).map(|d| make_report(1, d, shared_hash, 8.0, 14.0)).collect();
        let node_b: Vec<_> = (0..5).map(|d| make_report(2, d, shared_hash, 8.0, 20.0)).collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);

        // 5 days shared peer hash is below min_overlap_days (14), and mining hours
        // differ by 6 hours which is above the sync window. No alert expected.
        assert!(alerts.is_empty(), "5 days below threshold should not trigger alert");
    }

    #[test]
    fn test_different_locations_not_flagged() {
        let detector = ClusterDetector::default();

        // 30 days of data, but peer hashes are always different and mining times
        // are far apart. No farm signal at all.
        let node_a: Vec<_> = (0..30)
            .map(|d| {
                let mut hash = [0u8; 32];
                hash[0] = d as u8;
                hash[1] = 0xAA;
                make_report(1, d, hash, 8.0, 8.0)
            })
            .collect();

        let node_b: Vec<_> = (0..30)
            .map(|d| {
                let mut hash = [0u8; 32];
                hash[0] = d as u8;
                hash[1] = 0xBB;
                make_report(2, d, hash, 20.0, 20.0)
            })
            .collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);
        assert!(alerts.is_empty(), "Different locations and times should produce no alert");
    }

    #[test]
    fn test_confidence_calculation() {
        let detector = ClusterDetector::default();

        // 30 days of identical peer hashes + identical mining times for maximum confidence.
        let shared_hash = [0xFFu8; 32];

        let node_a: Vec<_> =
            (0..30).map(|d| make_report(1, d, shared_hash, 22.0, 22.0)).collect();
        let node_b: Vec<_> =
            (0..30).map(|d| make_report(2, d, shared_hash, 22.0, 22.0)).collect();

        let alerts = detector.analyze_reports(&[node_a, node_b]);

        assert_eq!(alerts.len(), 1);
        let alert = &alerts[0];

        // peer_confidence = min(30/30, 1.0) = 1.0
        // mining_confidence = 1.0 - 0.0/24.0 = 1.0
        // confidence = 1.0 * 1.0 = 1.0
        assert!(
            (alert.confidence - 1.0).abs() < f64::EPSILON,
            "Perfect overlap should yield confidence ~1.0, got {}",
            alert.confidence
        );

        assert_eq!(alert.cluster_type, ClusterType::FarmSignature);
        assert_eq!(alert.shared_peer_days, 30);
        assert!(alert.avg_mining_sync < f32::EPSILON);
    }
}
