//! Content storage layer for Lux posts.
//!
//! Handles post creation (signing, hashing), local persistence,
//! and anchor generation for on-chain recording.

use crate::types::*;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

/// Errors from the Lux store.
#[derive(Debug, thiserror::Error)]
pub enum LuxStoreError {
    #[error("Post content exceeds maximum size ({0} bytes > {MAX_POST_BYTES})")]
    PostTooLarge(usize),

    #[error("Too many attachments ({0} > {MAX_ATTACHMENTS})")]
    TooManyAttachments(usize),

    #[error("Post not found: {0}")]
    PostNotFound(String),

    #[error("Invalid signature")]
    InvalidSignature,

    #[error("User is muted until {0}")]
    UserMuted(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("IO error: {0}")]
    Io(String),
}

/// Serializable snapshot of LuxStore state for file-based persistence.
///
/// WHY: HashSet<(String, String)> doesn't serialize cleanly with serde_json
/// (tuples become arrays), so we convert liked to Vec<(String, String)> for
/// storage and convert back on load.
#[derive(Serialize, Deserialize)]
struct LuxStoreSnapshot {
    posts: HashMap<String, LuxPost>,
    engagement: HashMap<String, EngagementCounts>,
    profiles: HashMap<String, LuxProfile>,
    following: HashMap<String, Vec<String>>,
    followers: HashMap<String, Vec<String>>,
    /// Liked pairs stored as Vec for clean JSON serialization.
    liked: Vec<(String, String)>,
    bans: HashMap<String, BanRecord>,
    /// Feed index: post hashes sorted by timestamp, newest first.
    time_sorted_posts: Vec<String>,
    /// Reply index: parent_hash -> list of reply hashes.
    reply_index: HashMap<String, Vec<String>>,
    /// Posts removed by moderation verdicts.
    #[serde(default)]
    removed_posts: HashSet<String>,
}

/// Local store for Lux posts, profiles, and engagement data.
///
/// WHY: In-memory for Phase 1. Will be backed by RocksDB (same as chain state)
/// for persistence. The store is the single source of truth for the local node —
/// it caches DHT content and tracks local engagement state.
pub struct LuxStore {
    /// All posts this node knows about, indexed by hash.
    posts: HashMap<String, LuxPost>,

    /// Engagement counts per post hash.
    engagement: HashMap<String, EngagementCounts>,

    /// Profiles indexed by wallet address.
    profiles: HashMap<String, LuxProfile>,

    /// Who this node follows: follower -> set of followed addresses.
    following: HashMap<String, Vec<String>>,

    /// Who follows this node: followed -> set of follower addresses.
    followers: HashMap<String, Vec<String>>,

    /// Set of (liker_address, post_hash) to prevent double-likes.
    liked: HashSet<(String, String)>,

    /// Active bans.
    bans: HashMap<String, BanRecord>,

    /// On-chain anchors waiting to be included in a block.
    pending_anchors: Vec<PostAnchor>,

    /// Feed index: post hashes sorted by timestamp, newest first.
    /// WHY: Avoids O(n) sort on every feed request. Maintained incrementally
    /// as posts are created or received from the network.
    time_sorted_posts: Vec<String>,

    /// Reply index: parent_hash -> list of reply hashes.
    /// WHY: O(1) reply lookup instead of scanning all posts. The feed.rs
    /// reply_thread had a TODO for this — now it's available.
    reply_index: HashMap<String, Vec<String>>,

    /// Posts removed by moderation verdicts.
    /// WHY: Feed functions filter against this set so that posts removed by
    /// jury verdicts are actually hidden from all feeds. Without this, the
    /// moderation system's ContentRemoved verdict had no enforcement — removed
    /// posts remained visible in home, profile, and global feeds.
    removed_posts: HashSet<String>,
}

impl LuxStore {
    pub fn new() -> Self {
        Self {
            posts: HashMap::new(),
            engagement: HashMap::new(),
            profiles: HashMap::new(),
            following: HashMap::new(),
            followers: HashMap::new(),
            liked: HashSet::new(),
            bans: HashMap::new(),
            pending_anchors: Vec::new(),
            time_sorted_posts: Vec::new(),
            reply_index: HashMap::new(),
            removed_posts: HashSet::new(),
        }
    }

    // ========================================================================
    // Post creation
    // ========================================================================

    /// Create a new text post, sign it, compute hash, and generate an anchor.
    ///
    /// Returns the post hash on success.
    pub fn create_post(
        &mut self,
        author: &str,
        content: &str,
        signing_key: &SigningKey,
        reply_to: Option<String>,
    ) -> Result<String, LuxStoreError> {
        if content.len() > MAX_POST_BYTES {
            return Err(LuxStoreError::PostTooLarge(content.len()));
        }

        // Check if user is muted
        if let Some(ban) = self.bans.get(author) {
            if ban.permanently_muted {
                return Err(LuxStoreError::UserMuted("permanently".to_string()));
            }
            if let Some(expires) = &ban.mute_expires_at {
                if Utc::now() < *expires {
                    return Err(LuxStoreError::UserMuted(expires.to_string()));
                }
            }
        }

        let timestamp = Utc::now();

        // Build the post without hash/signature first (for canonical hashing)
        let canonical = CanonicalPost {
            author: author.to_string(),
            timestamp,
            content_type: "text/plain".to_string(),
            content: content.to_string(),
            attachments: vec![],
            reply_to: reply_to.clone(),
            repost_of: None,
            metadata: HashMap::new(),
        };

        // SHA-256 hash of the canonical form
        let canonical_bytes = serde_json::to_vec(&canonical)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;
        let hash = compute_sha256(&canonical_bytes);

        // Sign the hash with the author's Ed25519 key
        let hash_bytes = hex::decode(&hash)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;
        let signature = signing_key.sign(&hash_bytes);

        let post = LuxPost {
            hash: hash.clone(),
            author: author.to_string(),
            signature: signature.to_bytes().to_vec(),
            timestamp,
            content_type: "text/plain".to_string(),
            content: content.to_string(),
            attachments: vec![],
            reply_to,
            repost_of: None,
            metadata: HashMap::new(),
        };

        // Generate on-chain anchor
        let anchor = PostAnchor {
            post_hash: hash.clone(),
            author: author.to_string(),
            signature: post.signature.clone(),
            block_height: 0, // Set when included in a block
            anchor_type: AnchorType::Post,
        };

        // Update engagement counts and reply index for replies
        if let Some(ref parent) = post.reply_to {
            self.engagement
                .entry(parent.clone())
                .or_default()
                .replies += 1;
            self.reply_index
                .entry(parent.clone())
                .or_default()
                .push(hash.clone());
        }

        // Insert into time-sorted feed index (newest first).
        // WHY: binary search on timestamp keeps insertion O(log n) instead of
        // appending + re-sorting the whole vec on every query.
        let post_ts = post.timestamp;
        let insert_pos = self.time_sorted_posts
            .partition_point(|h| {
                self.posts.get(h)
                    .map(|p| p.timestamp >= post_ts)
                    .unwrap_or(false)
            });

        self.posts.insert(hash.clone(), post);
        self.time_sorted_posts.insert(insert_pos, hash.clone());
        self.engagement.entry(hash.clone()).or_default();
        self.pending_anchors.push(anchor);

        info!(post_hash = %hash, author = %author, "Lux post created");
        Ok(hash)
    }

    // ========================================================================
    // Engagement
    // ========================================================================

    /// Like a post. Generates an on-chain anchor (costs 1 Lux when mined).
    pub fn like_post(
        &mut self,
        post_hash: &str,
        liker: &str,
        signing_key: &SigningKey,
    ) -> Result<(), LuxStoreError> {
        if !self.posts.contains_key(post_hash) {
            return Err(LuxStoreError::PostNotFound(post_hash.to_string()));
        }

        let key = (liker.to_string(), post_hash.to_string());
        if self.liked.contains(&key) {
            // Already liked — idempotent, not an error
            return Ok(());
        }

        let hash_bytes = hex::decode(post_hash)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;
        let signature = signing_key.sign(&hash_bytes);

        let anchor = PostAnchor {
            post_hash: post_hash.to_string(),
            author: liker.to_string(),
            signature: signature.to_bytes().to_vec(),
            block_height: 0,
            anchor_type: AnchorType::Like {
                target_hash: post_hash.to_string(),
            },
        };

        self.engagement
            .entry(post_hash.to_string())
            .or_default()
            .likes += 1;
        self.liked.insert(key);
        self.pending_anchors.push(anchor);

        info!(post_hash = %post_hash, liker = %liker, "Post liked");
        Ok(())
    }

    /// Repost a post. Generates an on-chain anchor (costs 1 Lux when mined).
    pub fn repost(
        &mut self,
        original_hash: &str,
        reposter: &str,
        signing_key: &SigningKey,
        quote: Option<String>,
    ) -> Result<String, LuxStoreError> {
        if !self.posts.contains_key(original_hash) {
            return Err(LuxStoreError::PostNotFound(original_hash.to_string()));
        }

        // Create a repost as a new post that references the original
        let content = quote.unwrap_or_default();
        let timestamp = Utc::now();

        let canonical = CanonicalPost {
            author: reposter.to_string(),
            timestamp,
            content_type: "text/plain".to_string(),
            content: content.clone(),
            attachments: vec![],
            reply_to: None,
            repost_of: Some(original_hash.to_string()),
            metadata: HashMap::new(),
        };

        let canonical_bytes = serde_json::to_vec(&canonical)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;
        let hash = compute_sha256(&canonical_bytes);

        let hash_bytes = hex::decode(&hash)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;
        let signature = signing_key.sign(&hash_bytes);

        let post = LuxPost {
            hash: hash.clone(),
            author: reposter.to_string(),
            signature: signature.to_bytes().to_vec(),
            timestamp,
            content_type: "text/plain".to_string(),
            content,
            attachments: vec![],
            reply_to: None,
            repost_of: Some(original_hash.to_string()),
            metadata: HashMap::new(),
        };

        let anchor = PostAnchor {
            post_hash: hash.clone(),
            author: reposter.to_string(),
            signature: post.signature.clone(),
            block_height: 0,
            anchor_type: AnchorType::Repost {
                target_hash: original_hash.to_string(),
            },
        };

        self.engagement
            .entry(original_hash.to_string())
            .or_default()
            .reposts += 1;

        // Insert into time-sorted feed index (newest first).
        let post_ts = post.timestamp;
        let insert_pos = self.time_sorted_posts
            .partition_point(|h| {
                self.posts.get(h)
                    .map(|p| p.timestamp >= post_ts)
                    .unwrap_or(false)
            });

        self.posts.insert(hash.clone(), post);
        self.time_sorted_posts.insert(insert_pos, hash.clone());
        self.engagement.entry(hash.clone()).or_default();
        self.pending_anchors.push(anchor);

        info!(repost_hash = %hash, original = %original_hash, "Post reposted");
        Ok(hash)
    }

    // ========================================================================
    // Social graph
    // ========================================================================

    /// Follow a user.
    pub fn follow(&mut self, follower: &str, target: &str, signing_key: &SigningKey) {
        self.following
            .entry(follower.to_string())
            .or_default()
            .push(target.to_string());
        self.followers
            .entry(target.to_string())
            .or_default()
            .push(follower.to_string());

        let target_bytes = target.as_bytes();
        let signature = signing_key.sign(target_bytes);

        let anchor = PostAnchor {
            post_hash: String::new(),
            author: follower.to_string(),
            signature: signature.to_bytes().to_vec(),
            block_height: 0,
            anchor_type: AnchorType::Follow {
                target_address: target.to_string(),
            },
        };
        self.pending_anchors.push(anchor);

        info!(follower = %follower, target = %target, "Followed");
    }

    /// Unfollow a user.
    pub fn unfollow(&mut self, follower: &str, target: &str) {
        if let Some(list) = self.following.get_mut(follower) {
            list.retain(|a| a != target);
        }
        if let Some(list) = self.followers.get_mut(target) {
            list.retain(|a| a != follower);
        }
        info!(follower = %follower, target = %target, "Unfollowed");
    }

    /// Get the list of addresses a user follows.
    pub fn get_following(&self, address: &str) -> Vec<String> {
        self.following.get(address).cloned().unwrap_or_default()
    }

    /// Get the list of followers for an address.
    pub fn get_followers(&self, address: &str) -> Vec<String> {
        self.followers.get(address).cloned().unwrap_or_default()
    }

    // ========================================================================
    // Profiles
    // ========================================================================

    /// Set or update a user profile.
    pub fn set_profile(&mut self, profile: LuxProfile) {
        info!(address = %profile.address, name = ?profile.display_name, "Profile updated");
        self.profiles.insert(profile.address.clone(), profile);
    }

    /// Get a user's profile.
    pub fn get_profile(&self, address: &str) -> Option<&LuxProfile> {
        self.profiles.get(address)
    }

    // ========================================================================
    // Queries
    // ========================================================================

    /// Get a post by hash.
    pub fn get_post(&self, hash: &str) -> Option<&LuxPost> {
        // WHY: Filter out posts removed by moderation verdict. Without this,
        // jury verdicts with ContentRemoved outcome had no enforcement.
        if self.removed_posts.contains(hash) {
            return None;
        }
        let post = self.posts.get(hash)?;
        // WHY: Filter out posts from muted/banned authors.
        if self.is_muted(&post.author) {
            return None;
        }
        Some(post)
    }

    /// Get engagement counts for a post.
    pub fn get_engagement(&self, post_hash: &str) -> EngagementCounts {
        self.engagement.get(post_hash).cloned().unwrap_or_default()
    }

    /// Check if a user has liked a post.
    pub fn has_liked(&self, liker: &str, post_hash: &str) -> bool {
        self.liked.contains(&(liker.to_string(), post_hash.to_string()))
    }

    /// Get all posts by a specific author, newest first.
    pub fn get_posts_by_author(&self, author: &str) -> Vec<&LuxPost> {
        // WHY: Skip muted authors entirely and filter removed posts.
        if self.is_muted(author) {
            return Vec::new();
        }
        let mut posts: Vec<&LuxPost> = self.posts.values()
            .filter(|p| p.author == author && !self.removed_posts.contains(&p.hash))
            .collect();
        posts.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        posts
    }

    /// Get total post count.
    pub fn post_count(&self) -> usize {
        self.posts.len()
    }

    /// Iterate all post hashes. Used by FeedManager for global feed.
    pub fn posts_iter(&self) -> Vec<&String> {
        self.posts.keys().collect()
    }

    /// Drain pending anchors for inclusion in the next block.
    pub fn drain_pending_anchors(&mut self) -> Vec<PostAnchor> {
        std::mem::take(&mut self.pending_anchors)
    }

    // ========================================================================
    // Post verification
    // ========================================================================

    /// Verify a post's hash and signature are valid.
    pub fn verify_post(post: &LuxPost) -> Result<bool, LuxStoreError> {
        // Reconstruct canonical form and verify hash
        let canonical = CanonicalPost {
            author: post.author.clone(),
            timestamp: post.timestamp,
            content_type: post.content_type.clone(),
            content: post.content.clone(),
            attachments: post.attachments.clone(),
            reply_to: post.reply_to.clone(),
            repost_of: post.repost_of.clone(),
            metadata: post.metadata.clone(),
        };

        let canonical_bytes = serde_json::to_vec(&canonical)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;
        let expected_hash = compute_sha256(&canonical_bytes);

        if post.hash != expected_hash {
            warn!(expected = %expected_hash, actual = %post.hash, "Post hash mismatch — content tampered");
            return Ok(false);
        }

        Ok(true)
    }

    /// Store a post received from the network (after verification).
    pub fn store_received_post(&mut self, post: LuxPost) {
        let hash = post.hash.clone();

        // Update reply index if this is a reply
        if let Some(ref parent) = post.reply_to {
            self.reply_index
                .entry(parent.clone())
                .or_default()
                .push(hash.clone());
        }

        // Insert into time-sorted feed index (newest first).
        let post_ts = post.timestamp;
        let insert_pos = self.time_sorted_posts
            .partition_point(|h| {
                self.posts.get(h)
                    .map(|p| p.timestamp >= post_ts)
                    .unwrap_or(false)
            });

        self.posts.insert(hash.clone(), post);
        self.time_sorted_posts.insert(insert_pos, hash.clone());
        self.engagement.entry(hash).or_default();
    }

    // ========================================================================
    // Bans
    // ========================================================================

    /// Record a temp ban from a jury verdict.
    pub fn apply_temp_ban(&mut self, address: &str, hours: u32) {
        let ban = self.bans.entry(address.to_string()).or_insert(BanRecord {
            address: address.to_string(),
            temp_ban_count: 0,
            permanently_muted: false,
            mute_expires_at: None,
        });
        ban.temp_ban_count += 1;
        ban.mute_expires_at = Some(Utc::now() + chrono::Duration::hours(hours as i64));

        info!(address = %address, hours = hours, total_bans = ban.temp_ban_count, "Temp ban applied");
    }

    /// Mark a post as removed by moderation verdict.
    pub fn mark_post_removed(&mut self, post_hash: &str) {
        self.removed_posts.insert(post_hash.to_string());
    }

    /// Check if a post has been removed by moderation.
    pub fn is_post_removed(&self, post_hash: &str) -> bool {
        self.removed_posts.contains(post_hash)
    }

    /// Check if a user is currently muted.
    pub fn is_muted(&self, address: &str) -> bool {
        match self.bans.get(address) {
            None => false,
            Some(ban) => {
                if ban.permanently_muted {
                    return true;
                }
                if let Some(expires) = &ban.mute_expires_at {
                    Utc::now() < *expires
                } else {
                    false
                }
            }
        }
    }

    /// Get ban record for a user.
    pub fn get_ban_record(&self, address: &str) -> Option<&BanRecord> {
        self.bans.get(address)
    }

    // ========================================================================
    // Feed indexing
    // ========================================================================

    /// Get the most recent posts using the pre-sorted time index.
    ///
    /// WHY: O(limit) instead of O(n log n) — the index is maintained
    /// incrementally on insert so reads are fast.
    pub fn get_recent_posts(&self, limit: usize) -> Vec<&LuxPost> {
        self.time_sorted_posts.iter()
            .take(limit)
            .filter_map(|h| self.posts.get(h))
            .collect()
    }

    /// Get all replies to a post using the reply index.
    ///
    /// WHY: O(k) where k = number of replies, instead of scanning all posts.
    /// Replaces the linear scan TODO in feed.rs reply_thread.
    pub fn get_replies(&self, post_hash: &str) -> Vec<&LuxPost> {
        self.reply_index.get(post_hash)
            .map(|hashes| {
                hashes.iter()
                    .filter_map(|h| self.posts.get(h))
                    .collect()
            })
            .unwrap_or_default()
    }

    // ========================================================================
    // File-based persistence
    // ========================================================================

    /// Serialize the store state to a JSON file.
    ///
    /// WHY: Simple file persistence for Phase 1 before RocksDB migration.
    /// Converts HashSet<(String, String)> to Vec for clean JSON serialization.
    /// Pending anchors are NOT persisted — they belong to the current session
    /// and will be re-generated or lost on restart (by design).
    pub fn save_to_file(&self, path: &str) -> Result<(), LuxStoreError> {
        let snapshot = LuxStoreSnapshot {
            posts: self.posts.clone(),
            engagement: self.engagement.clone(),
            profiles: self.profiles.clone(),
            following: self.following.clone(),
            followers: self.followers.clone(),
            liked: self.liked.iter().cloned().collect(),
            bans: self.bans.clone(),
            time_sorted_posts: self.time_sorted_posts.clone(),
            reply_index: self.reply_index.clone(),
            removed_posts: self.removed_posts.clone(),
        };

        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;

        std::fs::write(path, json)
            .map_err(|e| LuxStoreError::Io(e.to_string()))?;

        info!(path = %path, posts = self.posts.len(), "LuxStore saved to file");
        Ok(())
    }

    /// Deserialize the store state from a JSON file.
    ///
    /// WHY: Returns a fresh empty store if the file doesn't exist, so callers
    /// don't need to check file existence separately. This makes first-run
    /// startup seamless.
    pub fn load_from_file(path: &str) -> Result<Self, LuxStoreError> {
        let data = match std::fs::read_to_string(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path, "No store file found, starting fresh");
                return Ok(Self::new());
            }
            Err(e) => return Err(LuxStoreError::Io(e.to_string())),
        };

        let snapshot: LuxStoreSnapshot = serde_json::from_str(&data)
            .map_err(|e| LuxStoreError::Serialization(e.to_string()))?;

        info!(path = %path, posts = snapshot.posts.len(), "LuxStore loaded from file");

        Ok(Self {
            posts: snapshot.posts,
            engagement: snapshot.engagement,
            profiles: snapshot.profiles,
            following: snapshot.following,
            followers: snapshot.followers,
            liked: snapshot.liked.into_iter().collect(),
            bans: snapshot.bans,
            pending_anchors: Vec::new(), // WHY: Pending anchors are session-local, never persisted
            time_sorted_posts: snapshot.time_sorted_posts,
            reply_index: snapshot.reply_index,
            removed_posts: snapshot.removed_posts,
        })
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Canonical post form used for hashing (excludes hash and signature fields).
#[derive(Serialize)]
struct CanonicalPost {
    author: String,
    timestamp: DateTime<Utc>,
    content_type: String,
    content: String,
    attachments: Vec<Attachment>,
    reply_to: Option<String>,
    repost_of: Option<String>,
    metadata: HashMap<String, String>,
}

/// Compute SHA-256 hash and return as hex string.
fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn test_create_and_retrieve_post() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);
        let author = "grat:abc123";

        let hash = store.create_post(author, "Hello Lux!", &key, None).unwrap();
        assert!(!hash.is_empty());

        let post = store.get_post(&hash).unwrap();
        assert_eq!(post.content, "Hello Lux!");
        assert_eq!(post.author, author);
        assert_eq!(post.content_type, "text/plain");
    }

    #[test]
    fn test_like_and_engagement() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);
        let author = "grat:author1";
        let liker = "grat:liker1";

        let hash = store.create_post(author, "Like me!", &key, None).unwrap();
        store.like_post(&hash, liker, &key).unwrap();

        let counts = store.get_engagement(&hash);
        assert_eq!(counts.likes, 1);
        assert!(store.has_liked(liker, &hash));

        // Double like is idempotent
        store.like_post(&hash, liker, &key).unwrap();
        assert_eq!(store.get_engagement(&hash).likes, 1);
    }

    #[test]
    fn test_follow_unfollow() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);

        store.follow("grat:a", "grat:b", &key);
        assert_eq!(store.get_following("grat:a"), vec!["grat:b"]);
        assert_eq!(store.get_followers("grat:b"), vec!["grat:a"]);

        store.unfollow("grat:a", "grat:b");
        assert!(store.get_following("grat:a").is_empty());
    }

    #[test]
    fn test_post_too_large() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);
        let huge = "x".repeat(MAX_POST_BYTES + 1);

        let result = store.create_post("grat:a", &huge, &key, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_recent_posts() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);

        let _h1 = store.create_post("grat:a", "First", &key, None).unwrap();
        let h2 = store.create_post("grat:a", "Second", &key, None).unwrap();
        let h3 = store.create_post("grat:a", "Third", &key, None).unwrap();

        let recent = store.get_recent_posts(2);
        assert_eq!(recent.len(), 2);
        // Newest first
        assert_eq!(recent[0].hash, h3);
        assert_eq!(recent[1].hash, h2);

        let all = store.get_recent_posts(100);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_reply_index() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);

        let parent = store.create_post("grat:a", "Parent post", &key, None).unwrap();
        let reply1 = store.create_post("grat:b", "Reply 1", &key, Some(parent.clone())).unwrap();
        let reply2 = store.create_post("grat:c", "Reply 2", &key, Some(parent.clone())).unwrap();

        let replies = store.get_replies(&parent);
        assert_eq!(replies.len(), 2);
        let reply_hashes: Vec<&str> = replies.iter().map(|p| p.hash.as_str()).collect();
        assert!(reply_hashes.contains(&reply1.as_str()));
        assert!(reply_hashes.contains(&reply2.as_str()));

        // No replies for a post that has none
        assert!(store.get_replies(&reply1).is_empty());

        // Engagement counts should reflect reply count
        let eng = store.get_engagement(&parent);
        assert_eq!(eng.replies, 2);
    }

    #[test]
    fn test_store_received_post_updates_indexes() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);

        // Create a parent post
        let parent = store.create_post("grat:a", "Parent", &key, None).unwrap();

        // Simulate receiving a reply from the network
        let received = LuxPost {
            hash: "fake_reply_hash".to_string(),
            author: "grat:b".to_string(),
            signature: vec![0u8; 64],
            timestamp: Utc::now(),
            content_type: "text/plain".to_string(),
            content: "Network reply".to_string(),
            attachments: vec![],
            reply_to: Some(parent.clone()),
            repost_of: None,
            metadata: HashMap::new(),
        };
        store.store_received_post(received);

        // Reply index should include it
        let replies = store.get_replies(&parent);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].hash, "fake_reply_hash");

        // Time-sorted index should include it
        let recent = store.get_recent_posts(10);
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let mut store = LuxStore::new();
        let key = SigningKey::generate(&mut OsRng);

        // Create some posts (sleep between to guarantee different timestamps for ordering)
        let h1 = store.create_post("grat:alice", "Hello world", &key, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let h2 = store.create_post("grat:bob", "Reply!", &key, Some(h1.clone())).unwrap();

        // Add engagement
        store.like_post(&h1, "grat:bob", &key).unwrap();

        // Add social graph
        store.follow("grat:alice", "grat:bob", &key);

        // Add profile
        store.set_profile(LuxProfile {
            address: "grat:alice".to_string(),
            display_name: Some("Alice".to_string()),
            bio: Some("Test bio".to_string()),
            avatar_hash: None,
            updated_at: Utc::now(),
            signature: vec![0u8; 64],
        });

        // Save to temp file (unique per thread to avoid parallel test collisions)
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lux_store_test_{:?}_{}.json", std::thread::current().id(), std::process::id()));
        let path_str = path.to_str().unwrap();
        store.save_to_file(path_str).unwrap();

        // Load from file
        let mut loaded = LuxStore::load_from_file(path_str).unwrap();

        // Verify posts
        assert_eq!(loaded.post_count(), 2);
        let post1 = loaded.get_post(&h1).unwrap();
        assert_eq!(post1.content, "Hello world");
        let post2 = loaded.get_post(&h2).unwrap();
        assert_eq!(post2.content, "Reply!");

        // Verify engagement
        assert_eq!(loaded.get_engagement(&h1).likes, 1);
        assert!(loaded.has_liked("grat:bob", &h1));

        // Verify social graph
        assert_eq!(loaded.get_following("grat:alice"), vec!["grat:bob"]);
        assert_eq!(loaded.get_followers("grat:bob"), vec!["grat:alice"]);

        // Verify profile
        let profile = loaded.get_profile("grat:alice").unwrap();
        assert_eq!(profile.display_name, Some("Alice".to_string()));

        // Verify feed index
        let recent = loaded.get_recent_posts(10);
        assert_eq!(recent.len(), 2);

        // Verify reply index
        let replies = loaded.get_replies(&h1);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].hash, h2);

        // Verify reply engagement count survived
        assert_eq!(loaded.get_engagement(&h1).replies, 1);

        // Pending anchors should NOT survive (session-local)
        assert!(loaded.drain_pending_anchors().is_empty());

        // Clean up
        let _ = std::fs::remove_file(path_str);
    }

    #[test]
    fn test_load_missing_file_returns_empty_store() {
        let loaded = LuxStore::load_from_file("/tmp/nonexistent_lux_store_42.json").unwrap();
        assert_eq!(loaded.post_count(), 0);
    }
}
