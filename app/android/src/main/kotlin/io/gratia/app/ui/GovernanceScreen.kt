package io.gratia.app.ui

import androidx.compose.foundation.BorderStroke
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.People
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Divider
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SuggestionChip
import androidx.compose.material3.SuggestionChipDefaults
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import io.gratia.app.GratiaLogo
import io.gratia.app.ui.theme.*
import kotlinx.coroutines.delay
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.TimeUnit

// ============================================================================
// GovernanceScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GovernanceScreen(
    viewModel: GovernanceViewModel = viewModel(),
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    // WHY: Auto-refresh every 30 seconds so vote counts and status changes
    // appear without requiring the user to manually pull-to-refresh.
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000L) // 30 seconds between refreshes
            viewModel.loadGovernanceData()
        }
    }

    // Detail views take over the entire screen
    when {
        state.selectedProposal != null -> {
            ProposalDetailScreen(
                proposal = state.selectedProposal!!,
                onBack = { viewModel.clearSelectedProposal() },
                onVote = { vote -> viewModel.voteOnProposal(state.selectedProposal!!.id, vote) },
            )
        }

        state.selectedPoll != null -> {
            PollDetailScreen(
                poll = state.selectedPoll!!,
                onBack = { viewModel.clearSelectedPoll() },
                onVote = { idx -> viewModel.voteOnPoll(state.selectedPoll!!.id, idx) },
            )
        }

        else -> {
            GovernanceListScreen(state, viewModel)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GovernanceListScreen(
    state: GovernanceUiState,
    viewModel: GovernanceViewModel,
) {
    var selectedTab by remember { mutableIntStateOf(0) }
    var showCreateDialog by remember { mutableStateOf(false) }
    val tabs = listOf("Proposals", "Polls")

    // Create dialog
    if (showCreateDialog) {
        CreateGovernanceDialog(
            selectedTab = selectedTab,
            participationDays = state.participationDays,
            canCreateProposal = state.canCreateProposal,
            walletBalanceLux = state.walletBalanceLux,
            onDismiss = { showCreateDialog = false },
            onCreatePoll = { question, options ->
                viewModel.createPoll(question, options)
                showCreateDialog = false
            },
            onCreateProposal = { title, description ->
                viewModel.createProposal(title, description)
                showCreateDialog = false
            },
        )
    }

    Scaffold(
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
                    "Governance",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = androidx.compose.ui.text.font.FontWeight.Bold,
                )
            }
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = { showCreateDialog = true },
            ) {
                Icon(Icons.Default.Add, contentDescription = "Create")
            }
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding)) {
            TabRow(selectedTabIndex = selectedTab) {
                tabs.forEachIndexed { index, title ->
                    Tab(
                        selected = selectedTab == index,
                        onClick = { selectedTab = index },
                        text = { Text(title) },
                    )
                }
            }

            if (state.isLoading) {
                Box(
                    modifier = Modifier.fillMaxSize(),
                    contentAlignment = Alignment.Center,
                ) {
                    CircularProgressIndicator()
                }
            } else {
                when (selectedTab) {
                    0 -> ProposalsList(
                        proposals = state.proposals,
                        onSelect = { viewModel.selectProposal(it) },
                    )

                    1 -> PollsList(
                        polls = state.polls,
                        onSelect = { viewModel.selectPoll(it) },
                    )
                }
            }
        }
    }
}

// ============================================================================
// Proposals List
// ============================================================================

@Composable
private fun ProposalsList(
    proposals: List<Proposal>,
    onSelect: (Proposal) -> Unit,
) {
    if (proposals.isEmpty()) {
        EmptyState(
            title = "No proposals yet",
            subtitle = "Be the first to shape the network!",
        )
    } else {
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            itemsIndexed(proposals, key = { _, p -> p.id }) { index, proposal ->
                // WHY: Proposal numbers are displayed as 1-indexed, most recent first,
                // so #1 is the newest proposal (total - index).
                val proposalNumber = proposals.size - index
                ProposalCard(
                    proposal = proposal,
                    proposalNumber = proposalNumber,
                    onClick = { onSelect(proposal) },
                )
            }
        }
    }
}

@Composable
private fun ProposalCard(
    proposal: Proposal,
    proposalNumber: Int,
    onClick: () -> Unit,
) {
    val totalVotes = proposal.votesFor + proposal.votesAgainst + proposal.votesAbstain
    val statusColor = proposalStatusColor(proposal.status)
    val statusText = proposalStatusText(proposal)

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            // Timeline indicator row
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                // Colored dot timeline indicator
                TimelineDots(status = proposal.status)
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = statusText,
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.Medium,
                    color = statusColor,
                )
                Spacer(modifier = Modifier.weight(1f))
                Text(
                    text = "#$proposalNumber",
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
                )
            }

            Spacer(modifier = Modifier.height(8.dp))

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = proposal.title,
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }

            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "Proposed by ${truncateAddress(proposal.submittedByAddress)}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
            )

            if (totalVotes > 0) {
                Spacer(modifier = Modifier.height(8.dp))
                VoteBar(
                    votesFor = proposal.votesFor,
                    votesAgainst = proposal.votesAgainst,
                    votesAbstain = proposal.votesAbstain,
                )
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    text = "$totalVotes total votes",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                )
            }
        }
    }
}

/** Three dots showing the proposal lifecycle: Discussion -> Voting -> Result */
@Composable
private fun TimelineDots(status: String) {
    val discussionColor = when (status) {
        "discussion" -> CharcoalNavy
        else -> CharcoalNavy // Already past discussion
    }
    val votingColor = when (status) {
        "voting" -> AmberGold
        "passed", "rejected", "implemented" -> AmberGold
        else -> MaterialTheme.colorScheme.surfaceVariant
    }
    val resultColor = when (status) {
        "passed", "implemented" -> SignalGreen
        "rejected" -> AlertRed
        else -> MaterialTheme.colorScheme.surfaceVariant
    }

    Row(verticalAlignment = Alignment.CenterVertically) {
        TimelineDot(color = discussionColor, filled = true)
        Box(
            modifier = Modifier
                .width(12.dp)
                .height(2.dp)
                .background(
                    if (status != "discussion") votingColor.copy(alpha = 0.5f)
                    else MaterialTheme.colorScheme.surfaceVariant
                ),
        )
        TimelineDot(
            color = votingColor,
            filled = status != "discussion",
        )
        Box(
            modifier = Modifier
                .width(12.dp)
                .height(2.dp)
                .background(
                    if (status in listOf("passed", "rejected", "implemented")) resultColor.copy(alpha = 0.5f)
                    else MaterialTheme.colorScheme.surfaceVariant
                ),
        )
        TimelineDot(
            color = resultColor,
            filled = status in listOf("passed", "rejected", "implemented"),
        )
    }
}

@Composable
private fun TimelineDot(color: Color, filled: Boolean) {
    Box(
        modifier = Modifier
            .size(10.dp)
            .clip(CircleShape)
            .background(if (filled) color else color.copy(alpha = 0.2f)),
    )
}

/** Human-friendly status text with time remaining for active phases. */
private fun proposalStatusText(proposal: Proposal): String {
    val now = System.currentTimeMillis()
    return when (proposal.status) {
        "discussion" -> {
            val diff = proposal.discussionEndMillis - now
            if (diff > 0) {
                val days = TimeUnit.MILLISECONDS.toDays(diff)
                "Discussion: ${days}d left"
            } else {
                "Discussion ended"
            }
        }
        "voting" -> {
            val diff = proposal.votingEndMillis - now
            if (diff > 0) {
                val days = TimeUnit.MILLISECONDS.toDays(diff)
                "Voting: ${days}d left"
            } else {
                "Voting ended"
            }
        }
        "passed" -> "Passed"
        "rejected" -> "Rejected"
        "implemented" -> "Implemented"
        else -> proposal.status.replaceFirstChar { it.uppercase() }
    }
}

@Composable
private fun ProposalStatusChip(status: String, color: Color) {
    SuggestionChip(
        onClick = {},
        label = {
            Text(
                text = status.replaceFirstChar { it.uppercase() },
                style = MaterialTheme.typography.labelSmall,
            )
        },
        colors = SuggestionChipDefaults.suggestionChipColors(
            containerColor = color.copy(alpha = 0.12f),
            labelColor = color,
        ),
    )
}

@Composable
private fun VoteBar(
    votesFor: Int,
    votesAgainst: Int,
    votesAbstain: Int,
) {
    val total = (votesFor + votesAgainst + votesAbstain).coerceAtLeast(1)
    val forFraction = votesFor.toFloat() / total

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        LinearProgressIndicator(
            progress = forFraction,
            modifier = Modifier
                .weight(1f)
                .height(8.dp),
            color = SignalGreen,
            trackColor = MaterialTheme.colorScheme.error.copy(alpha = 0.4f),
        )
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = "${(forFraction * 100).toInt()}%",
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

private fun proposalStatusColor(status: String): Color = when (status) {
    "discussion" -> CharcoalNavy
    "voting" -> AmberGold
    "passed" -> SignalGreen
    "rejected" -> AlertRed
    "implemented" -> DarkGoldenrod
    else -> AgedGold
}

// ============================================================================
// Polls List
// ============================================================================

@Composable
private fun PollsList(
    polls: List<Poll>,
    onSelect: (Poll) -> Unit,
) {
    if (polls.isEmpty()) {
        EmptyState(
            title = "No polls yet",
            subtitle = "Create one to hear from the community!",
        )
    } else {
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            items(polls, key = { it.id }) { poll ->
                PollCard(poll = poll, onClick = { onSelect(poll) })
            }
        }
    }
}

@Composable
private fun PollCard(
    poll: Poll,
    onClick: () -> Unit,
) {
    val remaining = timeRemaining(poll.endMillis)

    // Find the leading option
    val maxVotes = poll.votes.maxOrNull() ?: 0
    val leadingIndex = if (maxVotes > 0) poll.votes.indexOf(maxVotes) else -1
    val leadingOption = if (leadingIndex >= 0) poll.options.getOrNull(leadingIndex) else null
    val leadingFraction = if (poll.totalVoters > 0 && maxVotes > 0) {
        maxVotes.toFloat() / poll.totalVoters
    } else {
        0f
    }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = poll.question,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
            )

            // Leading option preview
            if (leadingOption != null && poll.totalVoters > 0) {
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "Leading: $leadingOption (${(leadingFraction * 100).toInt()}%)",
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = FontWeight.Medium,
                    color = MaterialTheme.colorScheme.primary,
                )
                Spacer(modifier = Modifier.height(4.dp))
                LinearProgressIndicator(
                    progress = leadingFraction,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(6.dp),
                    color = MaterialTheme.colorScheme.primary,
                    trackColor = MaterialTheme.colorScheme.surfaceVariant,
                )
            }

            Spacer(modifier = Modifier.height(8.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        imageVector = Icons.Default.People,
                        contentDescription = "Voters",
                        modifier = Modifier.size(14.dp),
                        tint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        text = "${poll.totalVoters} voters",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                }
                Text(
                    text = remaining,
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = FontWeight.Medium,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }
    }
}

// ============================================================================
// Proposal Detail Screen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProposalDetailScreen(
    proposal: Proposal,
    onBack: () -> Unit,
    onVote: (String) -> Unit,
) {
    val dateFormat = remember { SimpleDateFormat("MMM d, yyyy", Locale.getDefault()) }
    val totalVotes = proposal.votesFor + proposal.votesAgainst + proposal.votesAbstain

    // Confirmation dialog state
    var confirmVote by remember { mutableStateOf<String?>(null) }

    // Show confirmation dialog before voting
    if (confirmVote != null) {
        val voteLabel = confirmVote!!.replaceFirstChar { it.uppercase() }
        val voteColor = when (confirmVote) {
            "for" -> SignalGreen
            "against" -> AlertRed
            else -> AgedGold
        }
        AlertDialog(
            onDismissRequest = { confirmVote = null },
            title = { Text("Confirm Your Vote") },
            text = {
                Column {
                    Text("You're voting $voteLabel on this proposal.")
                    Spacer(modifier = Modifier.height(4.dp))
                    Text(
                        text = "This cannot be changed.",
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = FontWeight.SemiBold,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        val vote = confirmVote!!
                        confirmVote = null
                        onVote(vote)
                    },
                    colors = ButtonDefaults.buttonColors(containerColor = voteColor),
                ) {
                    Text("Vote $voteLabel")
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmVote = null }) { Text("Cancel") }
            },
        )
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Proposal") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surface,
                ),
            )
        },
    ) { padding ->
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            // Title and status
            item {
                Column {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.Top,
                    ) {
                        Text(
                            text = proposal.title,
                            style = MaterialTheme.typography.headlineSmall,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        ProposalStatusChip(proposal.status, proposalStatusColor(proposal.status))
                    }
                    Spacer(modifier = Modifier.height(4.dp))
                    Text(
                        text = "Proposed by ${truncateAddress(proposal.submittedByAddress)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    )
                }
            }

            // Description
            item {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(
                            text = "Description",
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Spacer(modifier = Modifier.height(8.dp))
                        Text(
                            text = proposal.description,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
            }

            // Timeline
            item {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(
                            text = "Timeline",
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Spacer(modifier = Modifier.height(8.dp))
                        TimelineRow("Discussion ends", dateFormat.format(Date(proposal.discussionEndMillis)))
                        TimelineRow("Voting ends", dateFormat.format(Date(proposal.votingEndMillis)))
                    }
                }
            }

            // Vote results
            if (totalVotes > 0) {
                item {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text(
                                text = "Results",
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.SemiBold,
                            )
                            Spacer(modifier = Modifier.height(12.dp))
                            VoteResultRow("For", proposal.votesFor, totalVotes, SignalGreen)
                            VoteResultRow("Against", proposal.votesAgainst, totalVotes, MaterialTheme.colorScheme.error)
                            VoteResultRow("Abstain", proposal.votesAbstain, totalVotes, AgedGold)
                            Divider(modifier = Modifier.padding(vertical = 8.dp))
                            Text(
                                text = "$totalVotes total votes",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                            )
                        }
                    }
                }
            }

            // Vote buttons (only during voting phase)
            if (proposal.status == "voting") {
                item {
                    Card(
                        modifier = Modifier.fillMaxWidth(),
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.3f),
                        ),
                    ) {
                        Column(
                            modifier = Modifier.padding(16.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                        ) {
                            if (proposal.hasVotedOnProposal) {
                                // Already voted — show confirmation instead of buttons
                                Text(
                                    text = "You voted on this proposal",
                                    style = MaterialTheme.typography.titleSmall,
                                    fontWeight = FontWeight.SemiBold,
                                    color = MaterialTheme.colorScheme.primary,
                                )
                                Spacer(modifier = Modifier.height(4.dp))
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Icon(
                                        imageVector = Icons.Default.Check,
                                        contentDescription = null,
                                        modifier = Modifier.size(16.dp),
                                        tint = SignalGreen,
                                    )
                                    Spacer(modifier = Modifier.width(4.dp))
                                    Text(
                                        text = "Your vote has been recorded",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                                    )
                                }
                            } else {
                                Text(
                                    text = "Cast Your Vote",
                                    style = MaterialTheme.typography.titleSmall,
                                    fontWeight = FontWeight.SemiBold,
                                )
                                Text(
                                    text = "One phone, one vote",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                                )
                                Spacer(modifier = Modifier.height(4.dp))
                                Text(
                                    text = "Needs 51% to pass  \u00b7  20% quorum required",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
                                )
                                Spacer(modifier = Modifier.height(12.dp))
                                Row(
                                    modifier = Modifier.fillMaxWidth(),
                                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                                ) {
                                    // Green "For" button
                                    Button(
                                        onClick = { confirmVote = "for" },
                                        modifier = Modifier.weight(1f),
                                        colors = ButtonDefaults.buttonColors(
                                            containerColor = SignalGreen,
                                        ),
                                    ) {
                                        Text("For", color = Color.White)
                                    }
                                    // Red "Against" button
                                    Button(
                                        onClick = { confirmVote = "against" },
                                        modifier = Modifier.weight(1f),
                                        colors = ButtonDefaults.buttonColors(
                                            containerColor = AlertRed,
                                        ),
                                    ) {
                                        Text("Against", color = Color.White)
                                    }
                                    // Gray "Abstain" button
                                    Button(
                                        onClick = { confirmVote = "abstain" },
                                        modifier = Modifier.weight(1f),
                                        colors = ButtonDefaults.buttonColors(
                                            containerColor = AgedGold,
                                        ),
                                    ) {
                                        Text("Abstain", color = Color.White)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun TimelineRow(label: String, date: String) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
        )
        Text(
            text = date,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.Medium,
        )
    }
}

@Composable
private fun VoteResultRow(label: String, count: Int, total: Int, color: Color) {
    val fraction = if (total > 0) count.toFloat() / total else 0f

    Column(modifier = Modifier.padding(vertical = 4.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(
                text = label,
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = "$count (${(fraction * 100).toInt()}%)",
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
                color = color,
            )
        }
        Spacer(modifier = Modifier.height(4.dp))
        LinearProgressIndicator(
            progress = fraction,
            modifier = Modifier.fillMaxWidth(),
            color = color,
            trackColor = MaterialTheme.colorScheme.surfaceVariant,
        )
    }
}

// ============================================================================
// Poll Detail Screen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PollDetailScreen(
    poll: Poll,
    onBack: () -> Unit,
    onVote: (Int) -> Unit,
) {
    val remaining = timeRemaining(poll.endMillis)
    val maxVotes = poll.votes.maxOrNull() ?: 1

    // Selected option before confirming (null = no selection yet)
    var selectedOption by remember { mutableStateOf<Int?>(null) }
    // Confirmation dialog
    var confirmOption by remember { mutableStateOf<Int?>(null) }

    // Confirmation dialog before casting poll vote
    if (confirmOption != null) {
        val optionName = poll.options.getOrElse(confirmOption!!) { "Option" }
        AlertDialog(
            onDismissRequest = { confirmOption = null },
            title = { Text("Confirm Your Vote") },
            text = {
                Column {
                    Text("Vote for \"$optionName\"?")
                    Spacer(modifier = Modifier.height(4.dp))
                    Text(
                        text = "This cannot be changed.",
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = FontWeight.SemiBold,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.7f),
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        val idx = confirmOption!!
                        confirmOption = null
                        selectedOption = null
                        onVote(idx)
                    },
                ) {
                    Text("Confirm Vote")
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmOption = null }) { Text("Cancel") }
            },
        )
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Poll") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surface,
                ),
            )
        },
    ) { padding ->
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            // Question
            item {
                Text(
                    text = poll.question,
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                )
                Spacer(modifier = Modifier.height(4.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            imageVector = Icons.Default.People,
                            contentDescription = "Voters",
                            modifier = Modifier.size(14.dp),
                            tint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text(
                            text = "${poll.totalVoters} voters",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                        )
                    }
                    Text(
                        text = remaining,
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = FontWeight.Medium,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }

            // Results chart
            item {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(
                            text = "Results",
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Spacer(modifier = Modifier.height(12.dp))

                        poll.options.forEachIndexed { index, option ->
                            val votes = poll.votes.getOrElse(index) { 0 }
                            val fraction = if (poll.totalVoters > 0) {
                                votes.toFloat() / poll.totalVoters
                            } else {
                                0f
                            }
                            val isLeading = votes == maxVotes && maxVotes > 0

                            PollOptionResult(
                                option = option,
                                votes = votes,
                                fraction = fraction,
                                isLeading = isLeading,
                            )
                        }
                    }
                }
            }

            // Vote section
            item {
                Card(
                    modifier = Modifier.fillMaxWidth(),
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.3f),
                    ),
                ) {
                    Column(
                        modifier = Modifier.padding(16.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        if (poll.hasVotedOnPoll) {
                            // Already voted — show confirmation
                            Text(
                                text = "You voted on this poll",
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.SemiBold,
                                color = MaterialTheme.colorScheme.primary,
                            )
                            Spacer(modifier = Modifier.height(4.dp))
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Icon(
                                    imageVector = Icons.Default.Check,
                                    contentDescription = null,
                                    modifier = Modifier.size(16.dp),
                                    tint = SignalGreen,
                                )
                                Spacer(modifier = Modifier.width(4.dp))
                                Text(
                                    text = "Your vote has been recorded",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                                )
                            }
                        } else {
                            Text(
                                text = "Cast Your Vote",
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.SemiBold,
                            )
                            Text(
                                text = "One phone, one vote per poll",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                            )
                            Spacer(modifier = Modifier.height(12.dp))

                            poll.options.forEachIndexed { index, option ->
                                val isSelected = selectedOption == index
                                PollOptionSelectable(
                                    option = option,
                                    isSelected = isSelected,
                                    onClick = { selectedOption = index },
                                )
                                Spacer(modifier = Modifier.height(8.dp))
                            }

                            if (selectedOption != null) {
                                Spacer(modifier = Modifier.height(4.dp))
                                Button(
                                    onClick = { confirmOption = selectedOption },
                                    modifier = Modifier.fillMaxWidth(),
                                ) {
                                    Text("Vote for \"${poll.options.getOrElse(selectedOption!!) { "" }}\"")
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** Selectable card-style option for poll voting. */
@Composable
private fun PollOptionSelectable(
    option: String,
    isSelected: Boolean,
    onClick: () -> Unit,
) {
    val borderColor = if (isSelected) {
        MaterialTheme.colorScheme.primary
    } else {
        MaterialTheme.colorScheme.outline.copy(alpha = 0.4f)
    }
    val containerColor = if (isSelected) {
        MaterialTheme.colorScheme.primary.copy(alpha = 0.08f)
    } else {
        Color.Transparent
    }

    OutlinedCard(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
        border = BorderStroke(
            width = if (isSelected) 2.dp else 1.dp,
            color = borderColor,
        ),
        colors = CardDefaults.outlinedCardColors(containerColor = containerColor),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(
                text = option,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                modifier = Modifier.weight(1f),
            )
            if (isSelected) {
                Icon(
                    imageVector = Icons.Default.Check,
                    contentDescription = "Selected",
                    modifier = Modifier.size(20.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
            }
        }
    }
}

@Composable
private fun PollOptionResult(
    option: String,
    votes: Int,
    fraction: Float,
    isLeading: Boolean,
) {
    val barColor = if (isLeading) {
        MaterialTheme.colorScheme.primary
    } else {
        MaterialTheme.colorScheme.outline
    }

    Column(modifier = Modifier.padding(vertical = 4.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(
                text = option,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = if (isLeading) FontWeight.SemiBold else FontWeight.Normal,
            )
            Text(
                text = "$votes (${(fraction * 100).toInt()}%)",
                style = MaterialTheme.typography.bodySmall,
                fontWeight = FontWeight.Medium,
                color = barColor,
            )
        }
        Spacer(modifier = Modifier.height(4.dp))
        LinearProgressIndicator(
            progress = fraction,
            modifier = Modifier.fillMaxWidth(),
            color = barColor,
            trackColor = MaterialTheme.colorScheme.surfaceVariant,
        )
    }
}

// ============================================================================
// Create Dialog
// ============================================================================

// WHY: Poll creation burns 10 GRAT. This constant is the Lux equivalent
// so we can compare it against the user's wallet balance.
private const val POLL_CREATION_COST_LUX = 10_000_000L // 10 GRAT = 10,000,000 Lux
private const val TITLE_MAX_LENGTH = 100
private const val DESCRIPTION_MAX_LENGTH = 2000

@Composable
private fun CreateGovernanceDialog(
    selectedTab: Int,
    participationDays: Long,
    canCreateProposal: Boolean,
    walletBalanceLux: Long,
    onDismiss: () -> Unit,
    onCreatePoll: (question: String, options: List<String>) -> Unit,
    onCreateProposal: (title: String, description: String) -> Unit,
) {
    if (selectedTab == 1) {
        // Create Poll
        var question by remember { mutableStateOf("") }
        // WHY: Individual option fields instead of "one per line" textarea.
        // Regular users don't expect to type multiple items separated by
        // line breaks — they expect separate input fields like any form.
        val options = remember { mutableStateListOf("", "") }

        val canAfford = walletBalanceLux >= POLL_CREATION_COST_LUX
        val validOptions = options.count { it.trim().isNotEmpty() } >= 2
        val maxOptions = 10

        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Create Poll") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    // Cost and balance info
                    Card(
                        colors = CardDefaults.cardColors(
                            containerColor = if (canAfford) {
                                MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.3f)
                            } else {
                                AlertRed.copy(alpha = 0.1f)
                            },
                        ),
                    ) {
                        Column(modifier = Modifier.padding(12.dp)) {
                            Text(
                                text = "Creating a poll burns 10 GRAT",
                                style = MaterialTheme.typography.bodySmall,
                                fontWeight = FontWeight.SemiBold,
                            )
                            Text(
                                text = "Your balance: ${formatGrat(walletBalanceLux)} GRAT",
                                style = MaterialTheme.typography.bodySmall,
                                color = if (canAfford) {
                                    MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f)
                                } else {
                                    AlertRed
                                },
                            )
                            if (!canAfford) {
                                Text(
                                    text = "Insufficient balance",
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.SemiBold,
                                    color = AlertRed,
                                )
                            }
                        }
                    }

                    Text(
                        text = "One phone, one vote per poll",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                    OutlinedTextField(
                        value = question,
                        onValueChange = { if (it.length <= TITLE_MAX_LENGTH) question = it },
                        label = { Text("Question") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = false,
                        maxLines = 3,
                        supportingText = {
                            Text(
                                text = "${question.length}/$TITLE_MAX_LENGTH",
                                style = MaterialTheme.typography.labelSmall,
                                modifier = Modifier.fillMaxWidth(),
                                textAlign = TextAlign.End,
                            )
                        },
                    )

                    Text(
                        text = "Options",
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.Medium,
                    )

                    options.forEachIndexed { index, option ->
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            OutlinedTextField(
                                value = option,
                                onValueChange = { options[index] = it },
                                label = { Text("Option ${index + 1}") },
                                modifier = Modifier.weight(1f),
                                singleLine = true,
                            )
                            // Show remove button only if more than 2 options
                            if (options.size > 2) {
                                IconButton(
                                    onClick = { options.removeAt(index) },
                                ) {
                                    Text(
                                        "✕",
                                        color = MaterialTheme.colorScheme.error,
                                        fontWeight = FontWeight.Bold,
                                    )
                                }
                            }
                        }
                    }

                    if (options.size < maxOptions) {
                        TextButton(
                            onClick = { options.add("") },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text("+ Add Option")
                        }
                    } else {
                        Text(
                            text = "Maximum $maxOptions options",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                            modifier = Modifier.fillMaxWidth(),
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        val validOpts = options
                            .map { it.trim() }
                            .filter { it.isNotEmpty() }
                        if (question.isNotBlank() && validOpts.size >= 2) {
                            onCreatePoll(question.trim(), validOpts)
                        }
                    },
                    enabled = question.isNotBlank() && validOptions && canAfford,
                ) {
                    Text("Create Poll")
                }
            },
            dismissButton = {
                TextButton(onClick = onDismiss) { Text("Cancel") }
            },
        )
    } else {
        // Create Proposal (requires 90+ days PoL)
        var title by remember { mutableStateOf("") }
        var description by remember { mutableStateOf("") }

        val daysNeeded = (90 - participationDays).coerceAtLeast(0)

        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Create Proposal") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    // Eligibility info card
                    Card(
                        colors = CardDefaults.cardColors(
                            containerColor = if (canCreateProposal) {
                                SignalGreen.copy(alpha = 0.1f)
                            } else {
                                AmberGold.copy(alpha = 0.1f)
                            },
                        ),
                    ) {
                        Column(modifier = Modifier.padding(12.dp)) {
                            if (canCreateProposal) {
                                Text(
                                    text = "Requires 90+ days Proof of Life history",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                                )
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Icon(
                                        imageVector = Icons.Default.Check,
                                        contentDescription = null,
                                        modifier = Modifier.size(14.dp),
                                        tint = SignalGreen,
                                    )
                                    Spacer(modifier = Modifier.width(4.dp))
                                    Text(
                                        text = "You qualify ($participationDays days)",
                                        style = MaterialTheme.typography.bodySmall,
                                        fontWeight = FontWeight.SemiBold,
                                        color = SignalGreen,
                                    )
                                }
                            } else {
                                Text(
                                    text = "Requires 90+ days Proof of Life history",
                                    style = MaterialTheme.typography.bodySmall,
                                    fontWeight = FontWeight.SemiBold,
                                )
                                Text(
                                    text = "You need $daysNeeded more days of Proof of Life to submit proposals",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = AmberGold,
                                )
                                Spacer(modifier = Modifier.height(2.dp))
                                LinearProgressIndicator(
                                    progress = (participationDays.toFloat() / 90f).coerceIn(0f, 1f),
                                    modifier = Modifier.fillMaxWidth(),
                                    color = AmberGold,
                                    trackColor = MaterialTheme.colorScheme.surfaceVariant,
                                )
                                Text(
                                    text = "$participationDays / 90 days",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                                    modifier = Modifier.fillMaxWidth(),
                                    textAlign = TextAlign.End,
                                )
                            }
                        }
                    }

                    OutlinedTextField(
                        value = title,
                        onValueChange = { if (it.length <= TITLE_MAX_LENGTH) title = it },
                        label = { Text("Title") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = true,
                        enabled = canCreateProposal,
                        supportingText = {
                            Text(
                                text = "${title.length}/$TITLE_MAX_LENGTH",
                                style = MaterialTheme.typography.labelSmall,
                                modifier = Modifier.fillMaxWidth(),
                                textAlign = TextAlign.End,
                            )
                        },
                    )
                    OutlinedTextField(
                        value = description,
                        onValueChange = { if (it.length <= DESCRIPTION_MAX_LENGTH) description = it },
                        label = { Text("Description") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = false,
                        minLines = 4,
                        maxLines = 8,
                        enabled = canCreateProposal,
                        supportingText = {
                            Text(
                                text = "${description.length}/$DESCRIPTION_MAX_LENGTH",
                                style = MaterialTheme.typography.labelSmall,
                                modifier = Modifier.fillMaxWidth(),
                                textAlign = TextAlign.End,
                            )
                        },
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        if (title.isNotBlank() && description.isNotBlank()) {
                            onCreateProposal(title.trim(), description.trim())
                        }
                    },
                    enabled = canCreateProposal && title.isNotBlank() && description.isNotBlank(),
                ) {
                    Text("Submit Proposal")
                }
            },
            dismissButton = {
                TextButton(onClick = onDismiss) { Text("Cancel") }
            },
        )
    }
}

// ============================================================================
// Shared Components
// ============================================================================

@Composable
private fun EmptyState(title: String, subtitle: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(48.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = title,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                textAlign = TextAlign.Center,
            )
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = subtitle,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.4f),
                textAlign = TextAlign.Center,
            )
        }
    }
}

/** Format a future timestamp as a human-readable "time remaining" string. */
private fun timeRemaining(endMillis: Long): String {
    val diff = endMillis - System.currentTimeMillis()
    if (diff <= 0) return "Ended"

    val days = TimeUnit.MILLISECONDS.toDays(diff)
    val hours = TimeUnit.MILLISECONDS.toHours(diff) % 24

    return when {
        days > 0 -> "${days}d ${hours}h left"
        hours > 0 -> "${hours}h left"
        else -> "< 1h left"
    }
}
