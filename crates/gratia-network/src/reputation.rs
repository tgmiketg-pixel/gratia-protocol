//! # Peer Reputation and Rate Limiting
//!
//! Tracks peer behavior to protect the network from malicious or misbehaving nodes.
//!
//! ## Reputation System
//!
//! Every peer starts with a score of 100. Good behavior (relaying valid blocks
//! and transactions) increases the score; bad behavior (invalid data, spam)
//! decreases it. Peers whose score drops below thresholds are temporarily banned
//! or disconnected.
//!
//! ## Rate Limiting
//!
//! A sliding-window rate limiter prevents any single peer from flooding the node
//! with excessive messages, blocks, or transactions. Limits are configurable and
//! enforced per-peer per-action.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Duration, Utc};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Reputation constants
// ---------------------------------------------------------------------------

/// Starting reputation score for newly seen peers.
const DEFAULT_SCORE: i32 = 100;

/// Absolute minimum score. Scores are clamped to this floor.
const MIN_SCORE: i32 = -100;

/// Absolute maximum score. Scores are clamped to this ceiling.
const MAX_SCORE: i32 = 1000;

/// Score awarded for relaying a valid block.
/// WHY: Block relay is high-value work; generous reward encourages honest peers.
const VALID_BLOCK_REWARD: i32 = 5;

/// Score penalty for relaying an invalid block.
/// WHY: 4x the reward — invalid blocks waste validation resources on constrained phones.
const INVALID_BLOCK_PENALTY: i32 = -20;

/// Score awarded for relaying a valid transaction.
const VALID_TX_REWARD: i32 = 1;

/// Score penalty for relaying an invalid transaction.
/// WHY: 10x the reward — must strongly discourage tx spam on mobile bandwidth.
const INVALID_TX_PENALTY: i32 = -10;

/// Score penalty for sending a spam / unrecognized message.
/// WHY: Higher than invalid tx because spam has zero legitimate purpose.
const SPAM_PENALTY: i32 = -15;

/// Score threshold at which a peer should be disconnected.
const DISCONNECT_THRESHOLD: i32 = -50;

/// Duration of a short ban (score below disconnect threshold but above hard ban).
/// WHY: 1 hour is long enough to deter casual misbehavior but short enough
/// that a transiently buggy peer can recover the same day.
const SHORT_BAN_DURATION_HOURS: i64 = 1;

/// Duration of a hard ban (score at or below MIN_SCORE).
/// WHY: 24 hours is a full PoL cycle — forces the peer to wait a day before
/// reconnecting, matching the daily attestation cadence.
const HARD_BAN_DURATION_HOURS: i64 = 24;

// ---------------------------------------------------------------------------
// Rate limiting constants
// ---------------------------------------------------------------------------

/// Sliding window size for rate limiting.
/// WHY: 60 seconds balances burst tolerance against sustained flood protection.
const RATE_WINDOW_SECS: i64 = 60;

/// Maximum blocks a single peer may relay per window.
/// WHY: At 3-5 second block times, ~12-20 blocks/min is the theoretical max;
/// 10 allows normal operation while catching obvious floods.
const MAX_BLOCKS_PER_MINUTE: usize = 10;

/// Maximum transactions a single peer may relay per window.
/// WHY: Mobile nodes have limited bandwidth; 100 txs/min is generous for
/// legitimate relay while stopping spam torrents.
const MAX_TXS_PER_MINUTE: usize = 100;

/// Maximum generic messages a single peer may send per window.
/// WHY: Covers discovery pings, sync requests, etc. — 50/min is well above
/// normal operation but catches runaway peers.
const MAX_MESSAGES_PER_MINUTE: usize = 50;

// ---------------------------------------------------------------------------
// PeerReputation
// ---------------------------------------------------------------------------

/// Per-peer reputation state.
///
/// Tracks both the aggregate score and the individual event counters that
/// contributed to it, making it easy to inspect *why* a peer was penalized.
#[derive(Debug, Clone)]
pub struct PeerReputation {
    /// Aggregate reputation score, clamped to [`MIN_SCORE`]..=[`MAX_SCORE`].
    pub score: i32,

    /// Number of invalid blocks received from this peer.
    pub invalid_blocks_received: u32,

    /// Number of invalid transactions received from this peer.
    pub invalid_txs_received: u32,

    /// Number of spam / unrecognized messages received from this peer.
    pub spam_messages: u32,

    /// Number of valid blocks received from this peer.
    pub valid_blocks_received: u32,

    /// Timestamp of the last recorded activity from this peer.
    pub last_activity: DateTime<Utc>,

    /// If `Some`, the peer is banned until this time.
    pub banned_until: Option<DateTime<Utc>>,
}

impl PeerReputation {
    /// Create a new reputation entry with the default starting score.
    pub fn new() -> Self {
        Self {
            score: DEFAULT_SCORE,
            invalid_blocks_received: 0,
            invalid_txs_received: 0,
            spam_messages: 0,
            valid_blocks_received: 0,
            last_activity: Utc::now(),
            banned_until: None,
        }
    }

    /// Apply a score delta, clamping to the allowed range, and update the
    /// last-activity timestamp. Automatically evaluates ban status after
    /// every score change.
    fn apply_delta(&mut self, delta: i32) {
        self.score = (self.score + delta).clamp(MIN_SCORE, MAX_SCORE);
        self.last_activity = Utc::now();
        self.evaluate_ban();
    }

    /// Set or clear the ban based on current score.
    fn evaluate_ban(&mut self) {
        let now = Utc::now();

        // If already banned and ban hasn't expired, don't shorten it.
        if let Some(until) = self.banned_until {
            if until > now {
                // Score may have dropped further — extend the ban if warranted.
                let required_until = self.ban_duration_for_score(now);
                if let Some(new_until) = required_until {
                    if new_until > until {
                        self.banned_until = Some(new_until);
                    }
                }
                return;
            }
        }

        // Apply a fresh ban if score warrants it.
        self.banned_until = self.ban_duration_for_score(now);
    }

    /// Determine the appropriate ban expiry for the current score.
    fn ban_duration_for_score(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if self.score <= MIN_SCORE {
            // WHY: Hardest ban for worst offenders — full 24-hour lockout.
            Some(now + Duration::hours(HARD_BAN_DURATION_HOURS))
        } else if self.score < DISCONNECT_THRESHOLD {
            // WHY: Short ban gives buggy-but-not-malicious peers a way back.
            Some(now + Duration::hours(SHORT_BAN_DURATION_HOURS))
        } else {
            None
        }
    }

    /// Returns `true` if the peer is currently banned.
    pub fn is_banned(&self) -> bool {
        match self.banned_until {
            Some(until) => Utc::now() < until,
            None => false,
        }
    }
}

impl Default for PeerReputation {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ReputationManager
// ---------------------------------------------------------------------------

/// Manages reputation scores for all known peers.
///
/// The caller is responsible for invoking the `record_*` methods when
/// events are observed. The manager updates scores, applies bans, and
/// exposes query methods for the rest of the networking stack.
#[derive(Debug)]
pub struct ReputationManager {
    /// Peer ID (libp2p PeerId as string) to reputation mapping.
    peers: HashMap<String, PeerReputation>,
    /// NodeId (hex string) → PeerId mapping.
    /// WHY: Attackers can rotate PeerId freely, but NodeId is cryptographically
    /// bound to their Ed25519 key. When a gossip message carries a NodeId
    /// (e.g., block producer, node announcement), we link it to the PeerId so
    /// reputation applies across PeerId rotations.
    // TODO: Full fix would key all reputation on NodeId instead of PeerId.
    // This partial fix links NodeId→PeerId so bans on a PeerId also apply
    // when the same NodeId appears from a different PeerId.
    node_to_peer: HashMap<String, String>,
}

impl ReputationManager {
    /// Create a new, empty reputation manager.
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
            node_to_peer: HashMap::new(),
        }
    }

    /// Get or create the reputation entry for a peer.
    ///
    /// WHY: Auto-evicts stale peers when the map exceeds 1000 entries to
    /// prevent unbounded memory growth on long-running nodes.
    fn entry(&mut self, peer_id: &str) -> &mut PeerReputation {
        if self.peers.len() > 1000 && !self.peers.contains_key(peer_id) {
            self.evict_stale_peers(chrono::Duration::hours(24));
        }
        self.peers
            .entry(peer_id.to_string())
            .or_insert_with(PeerReputation::new)
    }

    // ----- positive events -----

    /// Record that `peer_id` relayed a valid block.
    pub fn record_valid_block(&mut self, peer_id: &str) {
        let rep = self.entry(peer_id);
        rep.valid_blocks_received += 1;
        rep.apply_delta(VALID_BLOCK_REWARD);
        debug!(peer = peer_id, score = rep.score, "valid block, reputation +5");
    }

    /// Record that `peer_id` relayed a valid transaction.
    pub fn record_valid_tx(&mut self, peer_id: &str) {
        let rep = self.entry(peer_id);
        rep.apply_delta(VALID_TX_REWARD);
    }

    // ----- negative events -----

    /// Record that `peer_id` relayed an invalid block.
    pub fn record_invalid_block(&mut self, peer_id: &str) {
        let rep = self.entry(peer_id);
        rep.invalid_blocks_received += 1;
        rep.apply_delta(INVALID_BLOCK_PENALTY);
        warn!(peer = peer_id, score = rep.score, "invalid block, reputation -20");
    }

    /// Record that `peer_id` relayed an invalid transaction.
    pub fn record_invalid_tx(&mut self, peer_id: &str) {
        let rep = self.entry(peer_id);
        rep.invalid_txs_received += 1;
        rep.apply_delta(INVALID_TX_PENALTY);
        warn!(peer = peer_id, score = rep.score, "invalid tx, reputation -10");
    }

    /// Record that `peer_id` sent a spam or unrecognized message.
    pub fn record_spam(&mut self, peer_id: &str) {
        let rep = self.entry(peer_id);
        rep.spam_messages += 1;
        rep.apply_delta(SPAM_PENALTY);
        warn!(peer = peer_id, score = rep.score, "spam message, reputation -15");
    }

    // ----- queries -----

    /// Returns `true` if the peer is currently serving a ban.
    pub fn is_banned(&self, peer_id: &str) -> bool {
        self.peers
            .get(peer_id)
            .map_or(false, |r| r.is_banned())
    }

    /// Returns `true` if the peer's score is low enough to warrant
    /// disconnection (score < -50). A banned peer also returns `true`.
    pub fn should_disconnect(&self, peer_id: &str) -> bool {
        self.peers.get(peer_id).map_or(false, |r| {
            r.score < DISCONNECT_THRESHOLD || r.is_banned()
        })
    }

    /// Returns the current reputation score for a peer, or the default
    /// score if the peer is unknown.
    pub fn get_score(&self, peer_id: &str) -> i32 {
        self.peers
            .get(peer_id)
            .map_or(DEFAULT_SCORE, |r| r.score)
    }

    /// Returns a reference to the full reputation entry, if it exists.
    pub fn get_reputation(&self, peer_id: &str) -> Option<&PeerReputation> {
        self.peers.get(peer_id)
    }

    /// Remove peers that have been inactive for longer than `max_age`.
    /// Keeps the map from growing without bound on long-running nodes.
    pub fn evict_stale_peers(&mut self, max_age: Duration) {
        let cutoff = Utc::now() - max_age;
        self.peers.retain(|id, rep| {
            let keep = rep.last_activity > cutoff;
            if !keep {
                debug!(peer = id, "evicting stale reputation entry");
            }
            keep
        });
    }

    /// Link a NodeId (hex string) to a PeerId so that reputation and bans
    /// carry over when an attacker rotates their PeerId.
    ///
    /// If the NodeId was previously linked to a different PeerId that is banned,
    /// the ban is propagated to the new PeerId.
    pub fn link_node_id(&mut self, node_id: &str, peer_id: &str) {
        // Check if this NodeId was previously linked to a different PeerId
        // and transfer bad reputation if so.
        let should_transfer = self.node_to_peer.get(node_id)
            .filter(|old_peer| old_peer.as_str() != peer_id)
            .and_then(|old_peer| {
                self.peers.get(old_peer).cloned()
            })
            .filter(|old_rep| old_rep.score < DEFAULT_SCORE);

        if let Some(old_rep) = should_transfer {
            let new_rep = self.entry(peer_id);
            // Carry over the worse score if the old identity was penalized.
            if old_rep.score < new_rep.score {
                new_rep.score = old_rep.score;
                new_rep.banned_until = old_rep.banned_until;
                debug!(
                    node_id = node_id,
                    new_peer = peer_id,
                    score = old_rep.score,
                    "Transferred reputation from old PeerId to new PeerId for same NodeId",
                );
            }
        }
        self.node_to_peer.insert(node_id.to_string(), peer_id.to_string());
    }

    /// Check if a NodeId is banned (via its linked PeerId).
    pub fn is_node_banned(&self, node_id: &str) -> bool {
        self.node_to_peer
            .get(node_id)
            .map_or(false, |peer_id| self.is_banned(peer_id))
    }

    /// Number of tracked peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }
}

impl Default for ReputationManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RateLimiter
// ---------------------------------------------------------------------------

/// Per-action sliding-window rate limiter.
///
/// For each (peer, action) pair, maintains a deque of timestamps. A request
/// is allowed only if the number of timestamps within the last
/// [`RATE_WINDOW_SECS`] seconds is below the configured limit for that action.
#[derive(Debug)]
pub struct RateLimiter {
    /// (peer_id, action) -> ordered timestamps of events within the window.
    windows: HashMap<(String, String), VecDeque<DateTime<Utc>>>,

    /// action -> maximum allowed events per window.
    limits: HashMap<String, usize>,
}

impl RateLimiter {
    /// Create a rate limiter with the default Gratia limits.
    pub fn new() -> Self {
        let mut limits = HashMap::new();
        limits.insert("block".to_string(), MAX_BLOCKS_PER_MINUTE);
        limits.insert("tx".to_string(), MAX_TXS_PER_MINUTE);
        limits.insert("message".to_string(), MAX_MESSAGES_PER_MINUTE);

        Self {
            windows: HashMap::new(),
            limits,
        }
    }

    /// Create a rate limiter with custom per-action limits.
    ///
    /// Keys should match the `action` strings passed to [`check_rate`].
    pub fn with_limits(limits: HashMap<String, usize>) -> Self {
        Self {
            windows: HashMap::new(),
            limits,
        }
    }

    /// Check whether `peer_id` is allowed to perform `action` right now.
    ///
    /// Returns `true` if the action is within limits (and records the event).
    /// Returns `false` if the rate has been exceeded (event is NOT recorded).
    pub fn check_rate(&mut self, peer_id: &str, action: &str) -> bool {
        let now = Utc::now();
        let window_start = now - Duration::seconds(RATE_WINDOW_SECS);

        let key = (peer_id.to_string(), action.to_string());
        let timestamps = self.windows.entry(key).or_insert_with(VecDeque::new);

        // Evict expired entries from the front of the deque.
        while let Some(&front) = timestamps.front() {
            if front < window_start {
                timestamps.pop_front();
            } else {
                break;
            }
        }

        let limit = self.limits.get(action).copied().unwrap_or(MAX_MESSAGES_PER_MINUTE);

        if timestamps.len() >= limit {
            debug!(
                peer = peer_id,
                action = action,
                count = timestamps.len(),
                limit = limit,
                "rate limit exceeded"
            );
            false
        } else {
            timestamps.push_back(now);
            true
        }
    }

    /// Remove all state for a specific peer (e.g. on disconnect).
    pub fn clear_peer(&mut self, peer_id: &str) {
        self.windows.retain(|(id, _), _| id != peer_id);
    }

    /// Remove all entries whose most-recent timestamp is older than `max_age`.
    /// Prevents unbounded memory growth.
    pub fn evict_stale_entries(&mut self, max_age: Duration) {
        let cutoff = Utc::now() - max_age;
        self.windows.retain(|_, deque| {
            deque.back().map_or(false, |&ts| ts > cutoff)
        });
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ReputationManager tests ----

    #[test]
    fn new_peer_starts_at_default_score() {
        let mgr = ReputationManager::new();
        assert_eq!(mgr.get_score("peer1"), DEFAULT_SCORE);
    }

    #[test]
    fn valid_block_increases_score() {
        let mut mgr = ReputationManager::new();
        mgr.record_valid_block("peer1");
        assert_eq!(mgr.get_score("peer1"), DEFAULT_SCORE + VALID_BLOCK_REWARD);
    }

    #[test]
    fn invalid_block_decreases_score() {
        let mut mgr = ReputationManager::new();
        mgr.record_invalid_block("peer1");
        assert_eq!(mgr.get_score("peer1"), DEFAULT_SCORE + INVALID_BLOCK_PENALTY);
    }

    #[test]
    fn valid_tx_increases_score() {
        let mut mgr = ReputationManager::new();
        mgr.record_valid_tx("peer1");
        assert_eq!(mgr.get_score("peer1"), DEFAULT_SCORE + VALID_TX_REWARD);
    }

    #[test]
    fn invalid_tx_decreases_score() {
        let mut mgr = ReputationManager::new();
        mgr.record_invalid_tx("peer1");
        assert_eq!(mgr.get_score("peer1"), DEFAULT_SCORE + INVALID_TX_PENALTY);
    }

    #[test]
    fn spam_decreases_score() {
        let mut mgr = ReputationManager::new();
        mgr.record_spam("peer1");
        assert_eq!(mgr.get_score("peer1"), DEFAULT_SCORE + SPAM_PENALTY);
    }

    #[test]
    fn score_clamps_to_max() {
        let mut mgr = ReputationManager::new();
        // Push score way above max via many valid blocks
        for _ in 0..200 {
            mgr.record_valid_block("peer1");
        }
        assert_eq!(mgr.get_score("peer1"), MAX_SCORE);
    }

    #[test]
    fn score_clamps_to_min() {
        let mut mgr = ReputationManager::new();
        // Push score way below min via many invalid blocks
        for _ in 0..20 {
            mgr.record_invalid_block("peer1");
        }
        assert_eq!(mgr.get_score("peer1"), MIN_SCORE);
    }

    #[test]
    fn disconnect_threshold_triggers() {
        let mut mgr = ReputationManager::new();
        assert!(!mgr.should_disconnect("peer1"));

        // 8 invalid blocks: 100 + (8 * -20) = -60, below -50 threshold
        for _ in 0..8 {
            mgr.record_invalid_block("peer1");
        }
        assert!(mgr.should_disconnect("peer1"));
    }

    #[test]
    fn auto_ban_on_low_score() {
        let mut mgr = ReputationManager::new();

        // 8 invalid blocks: score = -60, below -50 → short ban
        for _ in 0..8 {
            mgr.record_invalid_block("peer1");
        }
        assert!(mgr.is_banned("peer1"));

        let rep = mgr.get_reputation("peer1").unwrap();
        assert!(rep.banned_until.is_some());
    }

    #[test]
    fn hard_ban_at_min_score() {
        let mut mgr = ReputationManager::new();

        // Drive score to -100 (MIN_SCORE)
        for _ in 0..20 {
            mgr.record_invalid_block("peer1");
        }
        assert_eq!(mgr.get_score("peer1"), MIN_SCORE);
        assert!(mgr.is_banned("peer1"));

        // Hard ban should be ~24 hours from now
        let rep = mgr.get_reputation("peer1").unwrap();
        let ban_end = rep.banned_until.unwrap();
        let hours_until_ban_end = (ban_end - Utc::now()).num_hours();
        assert!(hours_until_ban_end >= 23 && hours_until_ban_end <= 24);
    }

    #[test]
    fn unknown_peer_not_banned() {
        let mgr = ReputationManager::new();
        assert!(!mgr.is_banned("unknown"));
        assert!(!mgr.should_disconnect("unknown"));
    }

    #[test]
    fn peer_count_tracks_correctly() {
        let mut mgr = ReputationManager::new();
        assert_eq!(mgr.peer_count(), 0);

        mgr.record_valid_block("a");
        mgr.record_valid_block("b");
        mgr.record_spam("c");
        assert_eq!(mgr.peer_count(), 3);
    }

    // ---- RateLimiter tests ----

    #[test]
    fn within_limit_allows() {
        let mut rl = RateLimiter::new();
        for _ in 0..MAX_BLOCKS_PER_MINUTE {
            assert!(rl.check_rate("peer1", "block"));
        }
    }

    #[test]
    fn exceeding_limit_rejects() {
        let mut rl = RateLimiter::new();
        for _ in 0..MAX_BLOCKS_PER_MINUTE {
            assert!(rl.check_rate("peer1", "block"));
        }
        // 11th should be rejected
        assert!(!rl.check_rate("peer1", "block"));
    }

    #[test]
    fn different_peers_independent() {
        let mut rl = RateLimiter::new();
        for _ in 0..MAX_BLOCKS_PER_MINUTE {
            assert!(rl.check_rate("peer1", "block"));
        }
        // peer1 is at limit, but peer2 should be fine
        assert!(!rl.check_rate("peer1", "block"));
        assert!(rl.check_rate("peer2", "block"));
    }

    #[test]
    fn different_actions_independent() {
        let mut rl = RateLimiter::new();
        for _ in 0..MAX_BLOCKS_PER_MINUTE {
            assert!(rl.check_rate("peer1", "block"));
        }
        // Blocks exhausted, but txs should still work
        assert!(!rl.check_rate("peer1", "block"));
        assert!(rl.check_rate("peer1", "tx"));
    }

    #[test]
    fn unknown_action_uses_message_limit() {
        let mut rl = RateLimiter::new();
        for _ in 0..MAX_MESSAGES_PER_MINUTE {
            assert!(rl.check_rate("peer1", "custom_action"));
        }
        assert!(!rl.check_rate("peer1", "custom_action"));
    }

    #[test]
    fn clear_peer_resets_limits() {
        let mut rl = RateLimiter::new();
        for _ in 0..MAX_BLOCKS_PER_MINUTE {
            assert!(rl.check_rate("peer1", "block"));
        }
        assert!(!rl.check_rate("peer1", "block"));

        rl.clear_peer("peer1");
        assert!(rl.check_rate("peer1", "block"));
    }

    #[test]
    fn custom_limits() {
        let mut limits = HashMap::new();
        limits.insert("block".to_string(), 2);
        let mut rl = RateLimiter::with_limits(limits);

        assert!(rl.check_rate("peer1", "block"));
        assert!(rl.check_rate("peer1", "block"));
        assert!(!rl.check_rate("peer1", "block"));
    }
}
