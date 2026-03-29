package io.gratia.app.ui

import android.util.Log
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Repeat
import androidx.compose.material.icons.filled.ChatBubbleOutline
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.gratia.app.GratiaLogo
import io.gratia.app.ui.theme.AmberGold
import io.gratia.app.ui.theme.DeepNavy
import io.gratia.app.ui.theme.SignalGreen
import kotlinx.coroutines.launch
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.UUID

// ============================================================================
// Lux Screen — Decentralized Social Feed
// ============================================================================

/**
 * Data class for a Lux post in the UI layer.
 *
 * WHY: Separate from the Rust LuxPost type. This is what the Compose UI renders.
 * Will be populated from the FFI bridge once gratia-lux is wired through.
 */
data class LuxPostUi(
    val id: String,
    val authorAddress: String,
    val authorDisplayName: String?,
    val content: String,
    val timestamp: Long,
    val likes: Int,
    val reposts: Int,
    val replies: Int,
    val likedByMe: Boolean,
    val repostedByMe: Boolean,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LuxScreen() {
    val posts = remember { mutableStateListOf<LuxPostUi>() }
    var showComposeDialog by remember { mutableStateOf(false) }
    var postFeeLux by remember { mutableStateOf(1L) }
    var totalBurned by remember { mutableStateOf(0L) }

    // Reply threading state
    var replyTargetPost by remember { mutableStateOf<LuxPostUi?>(null) }

    // Content reporting state
    var reportTargetPost by remember { mutableStateOf<LuxPostUi?>(null) }

    // Profile state
    var profileName by remember { mutableStateOf("") }
    var profileBio by remember { mutableStateOf("") }
    var profilePostCount by remember { mutableStateOf(0L) }
    var profileFollowerCount by remember { mutableStateOf(0L) }
    var profileFollowingCount by remember { mutableStateOf(0L) }
    var showEditProfileDialog by remember { mutableStateOf(false) }

    // Snackbar for report confirmation
    val snackbarHostState = remember { SnackbarHostState() }
    val coroutineScope = rememberCoroutineScope()

    // Load feed from the Rust LuxStore via the bridge
    fun refreshFeed() {
        try {
            val bridge = io.gratia.app.bridge.GratiaCoreManager
            if (bridge.isInitialized) {
                val feed = bridge.luxGetGlobalFeed(50)
                posts.clear()
                posts.addAll(feed.posts.map { p ->
                    LuxPostUi(
                        id = p.hash,
                        authorAddress = p.author,
                        authorDisplayName = p.authorDisplayName.ifEmpty { null },
                        content = p.content,
                        timestamp = p.timestampMillis,
                        likes = p.likes.toInt(),
                        reposts = p.reposts.toInt(),
                        replies = p.replies.toInt(),
                        likedByMe = p.likedByMe,
                        repostedByMe = p.repostedByMe,
                    )
                })
                postFeeLux = feed.postFeeLux
                totalBurned = feed.totalBurnedLux

                // WHY: Load own profile so the card at the top of the feed
                // reflects current display name, bio, and counters.
                // Uses the local wallet address to fetch the user's own profile.
                try {
                    val myAddress = bridge.getWalletInfo().address
                    val profile = bridge.luxGetProfile(myAddress)
                    profileName = profile.displayName
                    profileBio = profile.bio
                    profilePostCount = profile.postCount
                    profileFollowerCount = profile.followerCount
                    profileFollowingCount = profile.followingCount
                } catch (_: Exception) {}
            }
        } catch (_: Exception) {}
    }

    // Initial load + periodic refresh
    androidx.compose.runtime.LaunchedEffect(Unit) {
        refreshFeed()
        while (true) {
            kotlinx.coroutines.delay(5000)
            refreshFeed()
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(hostState = snackbarHostState) },
        topBar = {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                GratiaLogo(size = 56)
                Spacer(modifier = Modifier.width(12.dp))
                Text(
                    "Lux",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.Bold,
                )
                Spacer(modifier = Modifier.weight(1f))
                IconButton(onClick = { showEditProfileDialog = true }) {
                    Icon(
                        Icons.Default.Person,
                        contentDescription = "Edit profile",
                        tint = AmberGold,
                    )
                }
            }
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = { showComposeDialog = true },
                containerColor = AmberGold,
                contentColor = DeepNavy,
            ) {
                Icon(Icons.Default.Add, contentDescription = "New Post")
            }
        },
    ) { padding ->
        LazyColumn(
            contentPadding = PaddingValues(
                start = 16.dp,
                end = 16.dp,
                top = 16.dp + padding.calculateTopPadding(),
                bottom = 80.dp + padding.calculateBottomPadding(),
            ),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // Profile card at top of feed
            item(key = "__profile_card__") {
                LuxProfileCard(
                    displayName = profileName,
                    bio = profileBio,
                    postCount = profilePostCount,
                    followerCount = profileFollowerCount,
                    followingCount = profileFollowingCount,
                    onEditClick = { showEditProfileDialog = true },
                )
            }

            // Post count & burn stats bar
            item(key = "__stats_bar__") {
                LuxFeedStatsBar(
                    postCount = posts.size,
                    totalBurnedLux = totalBurned,
                )
            }

            items(posts.toList(), key = { it.id }) { post ->
                LuxPostCard(
                    post = post,
                    onClick = { replyTargetPost = post },
                    onReport = { reportTargetPost = post },
                    onLike = {
                        try {
                            val bridge = io.gratia.app.bridge.GratiaCoreManager
                            if (bridge.isInitialized) {
                                bridge.luxLikePost(post.id)
                            }
                        } catch (_: Exception) {}
                        val index = posts.indexOfFirst { it.id == post.id }
                        if (index >= 0) {
                            val current = posts[index]
                            posts[index] = current.copy(
                                likedByMe = !current.likedByMe,
                                likes = if (current.likedByMe) current.likes - 1 else current.likes + 1,
                            )
                        }
                    },
                    onRepost = {
                        try {
                            val bridge = io.gratia.app.bridge.GratiaCoreManager
                            if (bridge.isInitialized) {
                                bridge.luxRepost(post.id, null)
                            }
                        } catch (_: Exception) {}
                        val index = posts.indexOfFirst { it.id == post.id }
                        if (index >= 0) {
                            val current = posts[index]
                            posts[index] = current.copy(
                                repostedByMe = !current.repostedByMe,
                                reposts = if (current.repostedByMe) current.reposts - 1 else current.reposts + 1,
                            )
                        }
                    },
                    onReply = { replyTargetPost = post },
                )
            }
        }
    }

    // Compose new post dialog
    if (showComposeDialog) {
        ComposePostDialog(
            postFeeLux = postFeeLux,
            onPost = { content ->
                try {
                    val bridge = io.gratia.app.bridge.GratiaCoreManager
                    if (bridge.isInitialized) {
                        val hash = bridge.luxCreatePost(content)
                        // Refresh feed to show the new post
                        refreshFeed()
                    }
                } catch (e: Exception) {
                    Log.e("LuxScreen", "Failed to create post: ${e.message}")
                }
                showComposeDialog = false
            },
            onDismiss = { showComposeDialog = false },
        )
    }

    // Reply thread bottom sheet
    replyTargetPost?.let { post ->
        ReplyThreadBottomSheet(
            post = post,
            onDismiss = { replyTargetPost = null },
            onReply = { content ->
                try {
                    val bridge = io.gratia.app.bridge.GratiaCoreManager
                    if (bridge.isInitialized) {
                        bridge.luxReply(post.id, content)
                        refreshFeed()
                    }
                } catch (e: Exception) {
                    Log.e("LuxScreen", "Failed to reply: ${e.message}")
                }
                replyTargetPost = null
            },
        )
    }

    // Content report dialog
    reportTargetPost?.let { post ->
        ReportPostDialog(
            post = post,
            onDismiss = { reportTargetPost = null },
            onSubmit = { reason ->
                // WHY: Bridge integration for reporting comes later; for now log and confirm via snackbar
                Log.i("LuxScreen", "Report submitted for post ${post.id}: $reason")
                reportTargetPost = null
                coroutineScope.launch {
                    snackbarHostState.showSnackbar("Report submitted")
                }
            },
        )
    }

    // Edit profile dialog
    if (showEditProfileDialog) {
        EditProfileDialog(
            currentName = profileName,
            currentBio = profileBio,
            onSave = { newName, newBio ->
                try {
                    val bridge = io.gratia.app.bridge.GratiaCoreManager
                    if (bridge.isInitialized) {
                        bridge.luxSetProfile(newName, newBio)
                        profileName = newName
                        profileBio = newBio
                    }
                } catch (e: Exception) {
                    Log.e("LuxScreen", "Failed to save profile: ${e.message}")
                }
                showEditProfileDialog = false
            },
            onDismiss = { showEditProfileDialog = false },
        )
    }
}

// ============================================================================
// Post Card
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LuxPostCard(
    post: LuxPostUi,
    onClick: () -> Unit = {},
    onReport: () -> Unit = {},
    onLike: () -> Unit,
    onRepost: () -> Unit,
    onReply: () -> Unit,
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surface,
        ),
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
    ) {
        Column(modifier = Modifier.padding(14.dp)) {
            // Author row
            Row(verticalAlignment = Alignment.CenterVertically) {
                // Avatar placeholder
                Box(
                    modifier = Modifier
                        .size(40.dp)
                        .clip(CircleShape)
                        .background(AmberGold.copy(alpha = 0.2f)),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Default.Person,
                        contentDescription = null,
                        tint = AmberGold,
                        modifier = Modifier.size(24.dp),
                    )
                }

                Spacer(modifier = Modifier.width(10.dp))

                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = post.authorDisplayName ?: truncateAddress(post.authorAddress),
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.Bold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = truncateAddress(post.authorAddress),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                        maxLines = 1,
                    )
                }

                // Timestamp
                Text(
                    text = formatTimeAgo(post.timestamp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
                )

                // Overflow menu (report)
                PostOverflowMenu(onReport = onReport)
            }

            Spacer(modifier = Modifier.height(10.dp))

            // Post content
            Text(
                text = post.content,
                style = MaterialTheme.typography.bodyMedium,
            )

            Spacer(modifier = Modifier.height(10.dp))

            // Engagement bar
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                // Reply
                EngagementButton(
                    icon = Icons.Default.ChatBubbleOutline,
                    count = post.replies,
                    active = false,
                    activeColor = MaterialTheme.colorScheme.primary,
                    onClick = onReply,
                )

                // Repost
                EngagementButton(
                    icon = Icons.Default.Repeat,
                    count = post.reposts,
                    active = post.repostedByMe,
                    activeColor = SignalGreen,
                    onClick = onRepost,
                )

                // Like
                EngagementButton(
                    icon = if (post.likedByMe) Icons.Default.Favorite else Icons.Default.FavoriteBorder,
                    count = post.likes,
                    active = post.likedByMe,
                    activeColor = AmberGold,
                    onClick = onLike,
                )
            }
        }
    }
}

@Composable
private fun EngagementButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    count: Int,
    active: Boolean,
    activeColor: androidx.compose.ui.graphics.Color,
    onClick: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .clip(RoundedCornerShape(8.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 4.dp),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = if (active) activeColor else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
            modifier = Modifier.size(18.dp),
        )
        if (count > 0) {
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = count.toString(),
                style = MaterialTheme.typography.labelSmall,
                color = if (active) activeColor else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
            )
        }
    }
}

// ============================================================================
// Compose Post Dialog
// ============================================================================

@Composable
private fun ComposePostDialog(
    postFeeLux: Long = 1,
    onPost: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var text by remember { mutableStateOf("") }
    val maxChars = 280

    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text("New Post", fontWeight = FontWeight.Bold)
        },
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = { if (it.length <= maxChars) text = it },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(150.dp),
                    placeholder = { Text("What's on your mind?") },
                    maxLines = 8,
                )

                Spacer(modifier = Modifier.height(8.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        text = "Fee: $postFeeLux Lux",
                        style = MaterialTheme.typography.labelSmall,
                        color = AmberGold,
                    )
                    Text(
                        text = "${text.length}/$maxChars",
                        style = MaterialTheme.typography.labelSmall,
                        color = if (text.length > maxChars - 20) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f)
                        },
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { onPost(text.trim()) },
                enabled = text.isNotBlank(),
            ) {
                Text("Post")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

// ============================================================================
// Helpers
// ============================================================================

private fun truncateAddress(address: String): String {
    if (address.length <= 16) return address
    return "${address.take(10)}...${address.takeLast(4)}"
}

private fun formatTimeAgo(timestampMillis: Long): String {
    val now = System.currentTimeMillis()
    val diff = now - timestampMillis
    val seconds = diff / 1000
    val minutes = seconds / 60
    val hours = minutes / 60
    val days = hours / 24

    return when {
        seconds < 60 -> "now"
        minutes < 60 -> "${minutes}m"
        hours < 24 -> "${hours}h"
        days < 7 -> "${days}d"
        else -> {
            val sdf = SimpleDateFormat("MMM d", Locale.getDefault())
            sdf.format(Date(timestampMillis))
        }
    }
}

// ============================================================================
// Profile Card
// ============================================================================

@Composable
private fun LuxProfileCard(
    displayName: String,
    bio: String,
    postCount: Long,
    followerCount: Long,
    followingCount: Long,
    onEditClick: () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surface,
        ),
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
    ) {
        Column(modifier = Modifier.padding(14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                // Avatar placeholder
                Box(
                    modifier = Modifier
                        .size(48.dp)
                        .clip(CircleShape)
                        .background(AmberGold.copy(alpha = 0.2f)),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Default.Person,
                        contentDescription = null,
                        tint = AmberGold,
                        modifier = Modifier.size(28.dp),
                    )
                }

                Spacer(modifier = Modifier.width(12.dp))

                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = displayName.ifEmpty { "Anonymous" },
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.Bold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (bio.isNotEmpty()) {
                        Text(
                            text = bio,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }

                IconButton(onClick = onEditClick) {
                    Icon(
                        Icons.Default.Edit,
                        contentDescription = "Edit profile",
                        tint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                        modifier = Modifier.size(20.dp),
                    )
                }
            }

            Spacer(modifier = Modifier.height(10.dp))

            // Counters row
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                ProfileStat(label = "Posts", count = postCount)
                ProfileStat(label = "Followers", count = followerCount)
                ProfileStat(label = "Following", count = followingCount)
            }
        }
    }
}

@Composable
private fun ProfileStat(label: String, count: Long) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(
            text = count.toString(),
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Bold,
        )
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
        )
    }
}

// ============================================================================
// Edit Profile Dialog
// ============================================================================

@Composable
private fun EditProfileDialog(
    currentName: String,
    currentBio: String,
    onSave: (name: String, bio: String) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by remember { mutableStateOf(currentName) }
    var bio by remember { mutableStateOf(currentBio) }
    // WHY: 160-char bio limit matches typical social platform short bios
    // and keeps on-chain profile data compact.
    val maxBioChars = 160

    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text("Edit Profile", fontWeight = FontWeight.Bold)
        },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text("Display Name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = bio,
                    onValueChange = { if (it.length <= maxBioChars) bio = it },
                    label = { Text("Bio") },
                    maxLines = 3,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(100.dp),
                )
                Text(
                    text = "${bio.length}/$maxBioChars",
                    style = MaterialTheme.typography.labelSmall,
                    color = if (bio.length > maxBioChars - 20) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f)
                    },
                    modifier = Modifier.fillMaxWidth(),
                    textAlign = androidx.compose.ui.text.style.TextAlign.End,
                )
            }
        },
        confirmButton = {
            Button(onClick = { onSave(name.trim(), bio.trim()) }) {
                Text("Save")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

// ============================================================================
// Post Count & Burn Stats Bar
// ============================================================================

@Composable
private fun LuxFeedStatsBar(
    postCount: Int,
    totalBurnedLux: Long,
) {
    Text(
        text = "$postCount posts \u00b7 $totalBurnedLux Lux burned",
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 4.dp, vertical = 2.dp),
    )
}

// ============================================================================
// Post Overflow Menu (Report)
// ============================================================================

@Composable
private fun PostOverflowMenu(onReport: () -> Unit) {
    var expanded by remember { mutableStateOf(false) }

    Box {
        IconButton(
            onClick = { expanded = true },
            modifier = Modifier.size(28.dp),
        ) {
            Icon(
                Icons.Default.MoreVert,
                contentDescription = "More options",
                tint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
                modifier = Modifier.size(18.dp),
            )
        }

        DropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false },
        ) {
            DropdownMenuItem(
                text = { Text("Report") },
                onClick = {
                    expanded = false
                    onReport()
                },
            )
        }
    }
}

// ============================================================================
// Reply Thread Bottom Sheet
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ReplyThreadBottomSheet(
    post: LuxPostUi,
    onDismiss: () -> Unit,
    onReply: (String) -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var replyText by remember { mutableStateOf("") }
    val maxChars = 280 // Same limit as posts

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp)
                .padding(bottom = 32.dp),
        ) {
            // Header
            Text(
                text = "Thread",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.padding(bottom = 12.dp),
            )

            // Original post (non-interactive summary)
            Card(
                modifier = Modifier.fillMaxWidth(),
                colors = CardDefaults.cardColors(
                    containerColor = MaterialTheme.colorScheme.surfaceVariant,
                ),
            ) {
                Column(modifier = Modifier.padding(12.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(
                            modifier = Modifier
                                .size(32.dp)
                                .clip(CircleShape)
                                .background(AmberGold.copy(alpha = 0.2f)),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(
                                Icons.Default.Person,
                                contentDescription = null,
                                tint = AmberGold,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                        Spacer(modifier = Modifier.width(8.dp))
                        Text(
                            text = post.authorDisplayName ?: truncateAddress(post.authorAddress),
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.Bold,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Spacer(modifier = Modifier.weight(1f))
                        Text(
                            text = formatTimeAgo(post.timestamp),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
                        )
                    }
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = post.content,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }

            Spacer(modifier = Modifier.height(12.dp))
            HorizontalDivider()
            Spacer(modifier = Modifier.height(8.dp))

            // Replies section (placeholder — bridge will populate later)
            Text(
                text = "Replies",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                modifier = Modifier.padding(bottom = 8.dp),
            )

            // WHY: Empty box with minimum height so the area is visible even with no replies yet
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 60.dp)
                    .padding(bottom = 12.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "No replies yet",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.3f),
                )
            }

            HorizontalDivider()
            Spacer(modifier = Modifier.height(8.dp))

            // Reply compose area
            OutlinedTextField(
                value = replyText,
                onValueChange = { if (it.length <= maxChars) replyText = it },
                modifier = Modifier
                    .fillMaxWidth()
                    .height(100.dp),
                placeholder = { Text("Write a reply...") },
                maxLines = 4,
            )

            Spacer(modifier = Modifier.height(4.dp))

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "${replyText.length}/$maxChars",
                    style = MaterialTheme.typography.labelSmall,
                    color = if (replyText.length > maxChars - 20) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f)
                    },
                )

                Button(
                    onClick = { onReply(replyText.trim()) },
                    enabled = replyText.isNotBlank(),
                ) {
                    Text("Reply")
                }
            }
        }
    }
}

// ============================================================================
// Report Post Dialog
// ============================================================================

/**
 * Report reasons available to users.
 * WHY: Predefined set keeps reports structured and actionable for moderation.
 */
private val REPORT_REASONS = listOf(
    "Spam",
    "Harassment",
    "Hate Speech",
    "Violence",
    "Misinformation",
    "Other",
)

@Composable
private fun ReportPostDialog(
    post: LuxPostUi,
    onDismiss: () -> Unit,
    onSubmit: (String) -> Unit,
) {
    var selectedReason by remember { mutableStateOf<String?>(null) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text("Report Post", fontWeight = FontWeight.Bold)
        },
        text = {
            Column {
                Text(
                    text = "Why are you reporting this post?",
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(bottom = 12.dp),
                )

                REPORT_REASONS.forEach { reason ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(8.dp))
                            .clickable { selectedReason = reason }
                            .padding(vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = selectedReason == reason,
                            onClick = { selectedReason = reason },
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        Text(
                            text = reason,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { selectedReason?.let { onSubmit(it) } },
                enabled = selectedReason != null,
            ) {
                Text("Submit Report")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}
