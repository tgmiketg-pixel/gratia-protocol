//! Feed assembly for Lux.
//!
//! Builds chronological feeds from posts by followed accounts.
//! V1 is strictly chronological — no algorithmic ranking.

use crate::store::LuxStore;
use crate::types::*;

/// A feed item — a post with its engagement counts and context.
#[derive(Debug, Clone)]
pub struct FeedItem {
    pub post: LuxPost,
    pub engagement: EngagementCounts,
    pub author_display_name: Option<String>,
    /// True if the current user has liked this post.
    pub liked_by_me: bool,
}

/// Assembles feeds from the local store.
pub struct FeedManager;

impl FeedManager {
    /// Build the home feed for a user: posts from people they follow, newest first.
    ///
    /// WHY: Strictly chronological. No algorithm decides what you see.
    /// This is a core design principle — Lux feeds are honest.
    pub fn home_feed(store: &LuxStore, user_address: &str, limit: usize) -> Vec<FeedItem> {
        let following = store.get_following(user_address);
        let mut items: Vec<FeedItem> = Vec::new();

        for address in &following {
            for post in store.get_posts_by_author(address) {
                let engagement = store.get_engagement(&post.hash);
                let profile = store.get_profile(address);
                items.push(FeedItem {
                    post: post.clone(),
                    engagement,
                    author_display_name: profile.and_then(|p| p.display_name.clone()),
                    liked_by_me: store.has_liked(user_address, &post.hash),
                });
            }
        }

        // Also include the user's own posts in their feed
        for post in store.get_posts_by_author(user_address) {
            let engagement = store.get_engagement(&post.hash);
            let profile = store.get_profile(user_address);
            items.push(FeedItem {
                post: post.clone(),
                engagement,
                author_display_name: profile.and_then(|p| p.display_name.clone()),
                liked_by_me: store.has_liked(user_address, &post.hash),
            });
        }

        // Sort by timestamp, newest first
        items.sort_by(|a, b| b.post.timestamp.cmp(&a.post.timestamp));
        items.truncate(limit);
        items
    }

    /// Build a user's profile feed: all their posts, newest first.
    pub fn profile_feed(store: &LuxStore, profile_address: &str, viewer_address: &str, limit: usize) -> Vec<FeedItem> {
        let posts = store.get_posts_by_author(profile_address);
        let profile = store.get_profile(profile_address);

        posts.into_iter()
            .take(limit)
            .map(|post| {
                let engagement = store.get_engagement(&post.hash);
                FeedItem {
                    post: post.clone(),
                    engagement,
                    author_display_name: profile.and_then(|p| p.display_name.clone()),
                    liked_by_me: store.has_liked(viewer_address, &post.hash),
                }
            })
            .collect()
    }

    /// Build a reply thread: all replies to a specific post.
    pub fn reply_thread(store: &LuxStore, post_hash: &str, viewer_address: &str) -> Vec<FeedItem> {
        let mut replies: Vec<FeedItem> = store.get_replies(post_hash)
            .into_iter()
            .map(|post| {
                let engagement = store.get_engagement(&post.hash);
                let profile = store.get_profile(&post.author);
                FeedItem {
                    post: post.clone(),
                    engagement,
                    author_display_name: profile.and_then(|p| p.display_name.clone()),
                    liked_by_me: store.has_liked(viewer_address, &post.hash),
                }
            })
            .collect();
        replies.sort_by(|a, b| a.post.timestamp.cmp(&b.post.timestamp));
        replies
    }

    /// Global feed: all posts on the network, newest first.
    /// WHY: "Explore" tab — lets users discover new accounts to follow.
    pub fn global_feed(store: &LuxStore, viewer_address: &str, limit: usize) -> Vec<FeedItem> {
        let mut items: Vec<FeedItem> = Vec::new();

        // Collect all posts we know about
        // WHY: V1 iterates the full store. For scale, maintain a time-sorted
        // index in RocksDB.
        for hash in store.posts_iter() {
            if let Some(post) = store.get_post(hash) {
                let engagement = store.get_engagement(&post.hash);
                let profile = store.get_profile(&post.author);
                items.push(FeedItem {
                    post: post.clone(),
                    engagement,
                    author_display_name: profile.and_then(|p| p.display_name.clone()),
                    liked_by_me: store.has_liked(viewer_address, &post.hash),
                });
            }
        }

        items.sort_by(|a, b| b.post.timestamp.cmp(&a.post.timestamp));
        items.truncate(limit);
        items
    }
}
