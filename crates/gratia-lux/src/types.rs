//! Core types for the Lux social protocol.
//!
//! Designed to be content-type agnostic: V1 ships with text/plain only,
//! but the same structures carry images, video, and audio without any
//! protocol-level changes. Clients ignore content types they don't support.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Post
// ============================================================================

/// Maximum post body length in bytes.
///
/// WHY: 1120 bytes accommodates 280 Unicode characters (up to 4 bytes each).
/// Matches the mental model of "a tweet" while being UTF-8 safe. Longer content
/// can use attachments or linked posts.
pub const MAX_POST_BYTES: usize = 1120;

/// Maximum number of attachments per post.
///
/// WHY: Keeps DHT storage bounded. V1 doesn't render attachments, but the
/// protocol allows them so future clients can display images without a
/// protocol upgrade.
pub const MAX_ATTACHMENTS: usize = 4;

/// Maximum display name length in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 30;

/// Maximum bio length in characters.
pub const MAX_BIO_CHARS: usize = 160;

/// A Lux post — the fundamental content unit.
///
/// Posts are signed by the author's Ed25519 wallet key, hashed with SHA-256,
/// and the hash + signature are anchored on-chain. The full post content lives
/// in the DHT and can be verified against the on-chain hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LuxPost {
    /// SHA-256 hash of the canonical serialized post (excluding this field).
    /// Computed at creation time. This is what goes on-chain.
    pub hash: String,

    /// Wallet address of the post author (e.g., "grat:abc123...").
    pub author: String,

    /// Ed25519 signature of the post hash, proving authorship.
    pub signature: Vec<u8>,

    /// When the post was created.
    pub timestamp: DateTime<Utc>,

    /// MIME content type. V1 only renders "text/plain".
    /// Future: "image/jpeg", "video/mp4", "audio/ogg", etc.
    pub content_type: String,

    /// The post body. For text/plain, this is the message.
    /// For media types, this is a caption (optional).
    pub content: String,

    /// Optional attachments (images, video, etc.).
    /// Each attachment is a content-addressed hash pointing to DHT storage.
    /// V1: always empty. Infrastructure ready for V2+.
    pub attachments: Vec<Attachment>,

    /// If this post is a reply, the hash of the parent post.
    pub reply_to: Option<String>,

    /// If this post is a repost (quote-post), the hash of the original.
    pub repost_of: Option<String>,

    /// Extensible metadata for future protocol features.
    /// WHY: Allows adding new fields (polls, location tags, topics)
    /// without breaking existing clients. Unknown keys are ignored.
    pub metadata: std::collections::HashMap<String, String>,
}

/// An attachment reference. The actual bytes live in content-addressed storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Content-addressed hash (SHA-256) of the attachment bytes.
    pub hash: String,

    /// MIME type of the attachment.
    pub content_type: String,

    /// Size in bytes. Clients use this to decide whether to auto-download.
    pub size_bytes: u64,

    /// Optional alt text for accessibility.
    pub alt_text: Option<String>,
}

// ============================================================================
// On-Chain Anchor
// ============================================================================

/// The minimal on-chain record that proves a post existed.
///
/// WHY: ~120 bytes per post. This is what lives on the Gratia blockchain
/// forever. The full content is in the DHT, but this anchor makes it
/// incorruptible — any tampering is detectable by comparing content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostAnchor {
    /// SHA-256 hash of the full post content.
    pub post_hash: String,

    /// Wallet address of the author.
    pub author: String,

    /// Ed25519 signature of the post hash.
    pub signature: Vec<u8>,

    /// Block height where this anchor was recorded.
    pub block_height: u64,

    /// Anchor type — distinguishes posts from likes, reposts, etc.
    pub anchor_type: AnchorType,
}

/// Types of on-chain social anchors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AnchorType {
    /// A new post was created.
    Post,
    /// A like on an existing post (1 Lux burned).
    Like { target_hash: String },
    /// A repost of an existing post (1 Lux burned).
    Repost { target_hash: String },
    /// A follow action.
    Follow { target_address: String },
    /// An unfollow action.
    Unfollow { target_address: String },
    /// A moderation report.
    Report { target_hash: String, reason: ReportReason },
    /// A jury verdict.
    Verdict { target_hash: String, outcome: VerdictOutcome },
}

// ============================================================================
// Profile
// ============================================================================

/// A Lux user profile. Stored in DHT, linked to wallet address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LuxProfile {
    /// Wallet address — the canonical identity.
    pub address: String,

    /// Optional display name (max 30 chars).
    pub display_name: Option<String>,

    /// Optional short bio (max 160 chars).
    pub bio: Option<String>,

    /// Content-addressed hash of profile picture (stored in DHT).
    /// V1: not rendered. Infrastructure ready.
    pub avatar_hash: Option<String>,

    /// When this profile was last updated.
    pub updated_at: DateTime<Utc>,

    /// Ed25519 signature of the profile data, proving ownership.
    pub signature: Vec<u8>,
}

// ============================================================================
// Engagement
// ============================================================================

/// A like on a post. Costs 1 Lux (burned — deflationary).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Like {
    /// Hash of the liked post.
    pub post_hash: String,

    /// Address of the user who liked.
    pub liker: String,

    /// Signature proving the like is authentic.
    pub signature: Vec<u8>,

    pub timestamp: DateTime<Utc>,
}

/// A repost. Costs 1 Lux (burned — deflationary).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repost {
    /// Hash of the reposted post.
    pub original_hash: String,

    /// Address of the user who reposted.
    pub reposter: String,

    /// Optional quote text added by the reposter.
    pub quote: Option<String>,

    pub signature: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

/// Engagement counts for a post.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngagementCounts {
    pub likes: u64,
    pub reposts: u64,
    pub replies: u64,
}

// ============================================================================
// Social Graph
// ============================================================================

/// A follow relationship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Follow {
    /// The follower's address.
    pub follower: String,

    /// The address being followed.
    pub following: String,

    pub signature: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

// ============================================================================
// Moderation
// ============================================================================

/// Reasons for reporting a post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReportReason {
    Spam,
    Harassment,
    HateSpeech,
    Violence,
    IllegalContent,
    Misinformation,
    Other(String),
}

/// A moderation report filed by a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// Hash of the reported post.
    pub post_hash: String,

    /// Address of the reporter.
    pub reporter: String,

    /// Why the post was reported.
    pub reason: ReportReason,

    /// Optional additional context.
    pub context: Option<String>,

    pub signature: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

/// Outcome of a jury deliberation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VerdictOutcome {
    /// Post is fine, no action taken.
    NotGuilty,
    /// Post removed from DHT propagation.
    ContentRemoved,
    /// Author temporarily muted (24 hours).
    TempBan { duration_hours: u32 },
    /// Governance proposal triggered for permanent action.
    EscalatedToGovernance,
}

/// A jury verdict on a reported post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// Hash of the reported post.
    pub post_hash: String,

    /// The jury members (wallet addresses) who voted.
    pub jurors: Vec<String>,

    /// Votes: true = guilty, false = not guilty.
    pub votes: Vec<bool>,

    /// The outcome determined by the vote tally.
    pub outcome: VerdictOutcome,

    /// Block height where the verdict was recorded.
    pub block_height: u64,

    pub timestamp: DateTime<Utc>,
}

/// Ban record for a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanRecord {
    /// Banned user's address.
    pub address: String,

    /// How many temp bans in the current 30-day window.
    pub temp_ban_count: u32,

    /// Whether the user is permanently muted (via governance).
    pub permanently_muted: bool,

    /// When the current temp ban expires (if any).
    pub mute_expires_at: Option<DateTime<Utc>>,
}
