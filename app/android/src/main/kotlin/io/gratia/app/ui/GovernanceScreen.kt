package io.gratia.app.ui

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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Divider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.TabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SuggestionChip
import androidx.compose.material3.SuggestionChipDefaults
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import java.text.SimpleDateFormat
import java.util.Date
import io.gratia.app.GratiaLogo
import java.util.Locale
import java.util.concurrent.TimeUnit
import io.gratia.app.ui.theme.*

// ============================================================================
// GovernanceScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GovernanceScreen(
    viewModel: GovernanceViewModel = viewModel(),
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

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
            TopAppBar(
                navigationIcon = { GratiaLogo(modifier = Modifier.padding(start = 12.dp)) },
                title = { Text("Governance") },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surface,
                ),
            )
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
        EmptyState("No proposals yet")
    } else {
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            items(proposals, key = { it.id }) { proposal ->
                ProposalCard(proposal = proposal, onClick = { onSelect(proposal) })
            }
        }
    }
}

@Composable
private fun ProposalCard(
    proposal: Proposal,
    onClick: () -> Unit,
) {
    val totalVotes = proposal.votesFor + proposal.votesAgainst + proposal.votesAbstain
    val statusColor = proposalStatusColor(proposal.status)

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
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
                Spacer(modifier = Modifier.width(8.dp))
                ProposalStatusChip(proposal.status, statusColor)
            }

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
        EmptyState("No polls yet")
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
            Spacer(modifier = Modifier.height(8.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    text = "${poll.options.size} options",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                )
                Text(
                    text = "${poll.totalVoters} voters",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                )
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
                        text = "by ${truncateAddress(proposal.submittedByAddress)}",
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
                            Spacer(modifier = Modifier.height(12.dp))
                            Row(
                                modifier = Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                OutlinedButton(
                                    onClick = { onVote("for") },
                                    modifier = Modifier.weight(1f),
                                ) {
                                    Text("For")
                                }
                                OutlinedButton(
                                    onClick = { onVote("against") },
                                    modifier = Modifier.weight(1f),
                                ) {
                                    Text("Against")
                                }
                                OutlinedButton(
                                    onClick = { onVote("abstain") },
                                    modifier = Modifier.weight(1f),
                                ) {
                                    Text("Abstain")
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
                ) {
                    Text(
                        text = "${poll.totalVoters} voters",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
                    )
                    Text(
                        text = remaining,
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = FontWeight.Medium,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }

            // Results chart (simple bar representation)
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

            // Vote buttons
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
                            OutlinedButton(
                                onClick = { onVote(index) },
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .padding(vertical = 4.dp),
                            ) {
                                Text(option)
                            }
                        }
                    }
                }
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

@Composable
private fun CreateGovernanceDialog(
    selectedTab: Int,
    onDismiss: () -> Unit,
    onCreatePoll: (question: String, options: List<String>) -> Unit,
    onCreateProposal: (title: String, description: String) -> Unit,
) {
    if (selectedTab == 1) {
        // Create Poll
        var question by remember { mutableStateOf("") }
        var optionsText by remember { mutableStateOf("") }

        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Create Poll") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        text = "One phone, one vote per poll",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                    OutlinedTextField(
                        value = question,
                        onValueChange = { question = it },
                        label = { Text("Question") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = false,
                        maxLines = 3,
                    )
                    OutlinedTextField(
                        value = optionsText,
                        onValueChange = { optionsText = it },
                        label = { Text("Options (one per line)") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = false,
                        minLines = 3,
                        maxLines = 6,
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        val options = optionsText.lines()
                            .map { it.trim() }
                            .filter { it.isNotEmpty() }
                        if (question.isNotBlank() && options.size >= 2) {
                            onCreatePoll(question.trim(), options)
                        }
                    },
                    enabled = question.isNotBlank() &&
                        optionsText.lines().count { it.trim().isNotEmpty() } >= 2,
                ) {
                    Text("Create Poll")
                }
            },
            dismissButton = {
                TextButton(onClick = onDismiss) { Text("Cancel") }
            },
        )
    } else {
        // Create Proposal (requires 90+ days PoL — enforcement is in the ViewModel)
        var title by remember { mutableStateOf("") }
        var description by remember { mutableStateOf("") }

        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Create Proposal") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        text = "Requires 90+ days Proof of Life history",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.6f),
                    )
                    OutlinedTextField(
                        value = title,
                        onValueChange = { title = it },
                        label = { Text("Title") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = true,
                    )
                    OutlinedTextField(
                        value = description,
                        onValueChange = { description = it },
                        label = { Text("Description") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = false,
                        minLines = 4,
                        maxLines = 8,
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
                    enabled = title.isNotBlank() && description.isNotBlank(),
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
private fun EmptyState(message: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(48.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = message,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f),
            textAlign = TextAlign.Center,
        )
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
