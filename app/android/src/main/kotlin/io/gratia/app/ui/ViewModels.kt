package io.gratia.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

// ============================================================================
// Data classes mirroring FFI types for the UI layer.
// In production these will come from the UniFFI-generated Kotlin bindings.
// For now they carry mock data so the UI can be developed independently.
// ============================================================================

data class WalletInfo(
    val address: String,
    val balanceLux: Long,
    val miningState: String,
)

data class TransactionInfo(
    val hashHex: String,
    val direction: String,
    val counterparty: String?,
    val amountLux: Long,
    val timestampMillis: Long,
    val status: String,
)

data class MiningStatus(
    val state: String,
    val batteryPercent: Int,
    val isPluggedIn: Boolean,
    val currentDayPolValid: Boolean,
    val presenceScore: Int,
)

data class ProofOfLifeStatus(
    val isValidToday: Boolean,
    val consecutiveDays: Long,
    val isOnboarded: Boolean,
    val parametersMet: List<String>,
)

data class StakeInfo(
    val nodeStakeLux: Long,
    val overflowAmountLux: Long,
    val totalCommittedLux: Long,
    val stakedAtMillis: Long,
    val meetsMinimum: Boolean,
)

data class Proposal(
    val id: String,
    val title: String,
    val description: String,
    val status: String,
    val votesFor: Int,
    val votesAgainst: Int,
    val votesAbstain: Int,
    val discussionEndMillis: Long,
    val votingEndMillis: Long,
    val submittedByAddress: String,
)

data class Poll(
    val id: String,
    val question: String,
    val options: List<String>,
    val votes: List<Int>,
    val endMillis: Long,
    val createdByAddress: String,
    val totalVoters: Int,
)

// ============================================================================
// Wallet UI state
// ============================================================================

data class WalletUiState(
    val walletInfo: WalletInfo? = null,
    val transactions: List<TransactionInfo> = emptyList(),
    val isLoading: Boolean = true,
    val isRefreshing: Boolean = false,
    val showSendDialog: Boolean = false,
    val showReceiveDialog: Boolean = false,
    val errorMessage: String? = null,
)

class WalletViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(WalletUiState())
    val uiState: StateFlow<WalletUiState> = _uiState.asStateFlow()

    init {
        loadWalletData()
    }

    fun loadWalletData() {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(isLoading = true, errorMessage = null)
            // Simulate network/bridge delay
            delay(300)
            _uiState.value = _uiState.value.copy(
                walletInfo = mockWalletInfo(),
                transactions = mockTransactions(),
                isLoading = false,
            )
        }
    }

    fun refresh() {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(isRefreshing = true)
            delay(500)
            _uiState.value = _uiState.value.copy(
                walletInfo = mockWalletInfo(),
                transactions = mockTransactions(),
                isRefreshing = false,
            )
        }
    }

    fun showSendDialog() {
        _uiState.value = _uiState.value.copy(showSendDialog = true)
    }

    fun hideSendDialog() {
        _uiState.value = _uiState.value.copy(showSendDialog = false)
    }

    fun showReceiveDialog() {
        _uiState.value = _uiState.value.copy(showReceiveDialog = true)
    }

    fun hideReceiveDialog() {
        _uiState.value = _uiState.value.copy(showReceiveDialog = false)
    }

    fun sendTransfer(toAddress: String, amountLux: Long) {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(showSendDialog = false)
            // In production: call GratiaNode.sendTransfer(toAddress, amountLux)
            delay(200)
            loadWalletData()
        }
    }

    // -- Mock data generators ------------------------------------------------

    private fun mockWalletInfo() = WalletInfo(
        address = "grat:a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
        balanceLux = 42_750_000L, // 42.75 GRAT
        miningState = "mining",
    )

    private fun mockTransactions(): List<TransactionInfo> {
        val now = System.currentTimeMillis()
        return listOf(
            TransactionInfo(
                hashHex = "abc123def456",
                direction = "received",
                counterparty = "grat:f1e2d3c4b5a6f1e2d3c4b5a6f1e2d3c4b5a6f1e2d3c4b5a6f1e2d3c4b5a6f1e2",
                amountLux = 5_000_000L,
                timestampMillis = now - 3_600_000L, // 1 hour ago
                status = "confirmed",
            ),
            TransactionInfo(
                hashHex = "789abc012def",
                direction = "sent",
                counterparty = "grat:1234567890ab1234567890ab1234567890ab1234567890ab1234567890ab1234",
                amountLux = 1_250_000L,
                timestampMillis = now - 86_400_000L, // 1 day ago
                status = "confirmed",
            ),
            TransactionInfo(
                hashHex = "def012abc345",
                direction = "received",
                counterparty = null,
                amountLux = 500_000L,
                timestampMillis = now - 172_800_000L, // 2 days ago
                status = "confirmed",
            ),
            TransactionInfo(
                hashHex = "456789abcdef",
                direction = "sent",
                counterparty = "grat:deadbeefcafe0123deadbeefcafe0123deadbeefcafe0123deadbeefcafe0123",
                amountLux = 100_000L,
                timestampMillis = now - 600_000L, // 10 min ago
                status = "pending",
            ),
        )
    }
}

// ============================================================================
// Mining UI state
// ============================================================================

data class MiningUiState(
    val miningStatus: MiningStatus? = null,
    val polStatus: ProofOfLifeStatus? = null,
    val isLoading: Boolean = true,
    val earningsToday: Long = 0L,
    val earningsThisWeek: Long = 0L,
    val earningsTotal: Long = 0L,
)

class MiningViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(MiningUiState())
    val uiState: StateFlow<MiningUiState> = _uiState.asStateFlow()

    init {
        loadMiningData()
    }

    fun loadMiningData() {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(isLoading = true)
            delay(300)
            _uiState.value = MiningUiState(
                miningStatus = mockMiningStatus(),
                polStatus = mockPolStatus(),
                isLoading = false,
                earningsToday = 2_150_000L,   // 2.15 GRAT
                earningsThisWeek = 14_750_000L, // 14.75 GRAT
                earningsTotal = 42_750_000L,  // 42.75 GRAT
            )
        }
    }

    fun startMining() {
        viewModelScope.launch {
            // In production: call GratiaNode.startMining()
            val current = _uiState.value.miningStatus ?: return@launch
            _uiState.value = _uiState.value.copy(
                miningStatus = current.copy(state = "mining")
            )
        }
    }

    fun stopMining() {
        viewModelScope.launch {
            // In production: call GratiaNode.stopMining()
            val current = _uiState.value.miningStatus ?: return@launch
            _uiState.value = _uiState.value.copy(
                miningStatus = current.copy(state = "proof_of_life")
            )
        }
    }

    private fun mockMiningStatus() = MiningStatus(
        state = "mining",
        batteryPercent = 92,
        isPluggedIn = true,
        currentDayPolValid = true,
        presenceScore = 78,
    )

    private fun mockPolStatus() = ProofOfLifeStatus(
        isValidToday = true,
        consecutiveDays = 23,
        isOnboarded = true,
        parametersMet = listOf(
            "unlocks",
            "unlock_spread",
            "interactions",
            "orientation",
            "motion",
            "gps",
            "network",
            "bt_variation",
            "charge_event",
        ),
    )
}

// ============================================================================
// Settings UI state
// ============================================================================

data class SettingsUiState(
    val stakeInfo: StakeInfo? = null,
    val isLoading: Boolean = true,
    val nodeId: String = "",
    val appVersion: String = "0.1.0-alpha",
    val participationDays: Long = 0,
    val locationGranularity: LocationGranularity = LocationGranularity.CITY,
    val cameraHashEnabled: Boolean = false,
    val microphoneFingerprintEnabled: Boolean = false,
    val inheritanceEnabled: Boolean = false,
    val beneficiaryAddress: String = "",
    val showExportSeedConfirmation: Boolean = false,
    val showStakeDialog: Boolean = false,
    val showUnstakeDialog: Boolean = false,
    val showBeneficiaryDialog: Boolean = false,
)

enum class LocationGranularity(val label: String) {
    EXACT("Exact"),
    CITY("City-level"),
    REGION("Region-level"),
}

class SettingsViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(SettingsUiState())
    val uiState: StateFlow<SettingsUiState> = _uiState.asStateFlow()

    init {
        loadSettings()
    }

    fun loadSettings() {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(isLoading = true)
            delay(200)
            _uiState.value = SettingsUiState(
                stakeInfo = mockStakeInfo(),
                isLoading = false,
                nodeId = "grat:a1b2c3d4e5f6a1b2",
                appVersion = "0.1.0-alpha",
                participationDays = 23,
                locationGranularity = LocationGranularity.CITY,
                cameraHashEnabled = false,
                microphoneFingerprintEnabled = false,
                inheritanceEnabled = false,
                beneficiaryAddress = "",
            )
        }
    }

    fun showExportSeedConfirmation() {
        _uiState.value = _uiState.value.copy(showExportSeedConfirmation = true)
    }

    fun hideExportSeedConfirmation() {
        _uiState.value = _uiState.value.copy(showExportSeedConfirmation = false)
    }

    fun exportSeedPhrase() {
        // In production: call secure enclave to retrieve seed phrase
        _uiState.value = _uiState.value.copy(showExportSeedConfirmation = false)
    }

    fun showStakeDialog() {
        _uiState.value = _uiState.value.copy(showStakeDialog = true)
    }

    fun hideStakeDialog() {
        _uiState.value = _uiState.value.copy(showStakeDialog = false)
    }

    fun stake(amountLux: Long) {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(showStakeDialog = false)
            // In production: call GratiaNode.stake(amountLux)
            delay(200)
            loadSettings()
        }
    }

    fun showUnstakeDialog() {
        _uiState.value = _uiState.value.copy(showUnstakeDialog = true)
    }

    fun hideUnstakeDialog() {
        _uiState.value = _uiState.value.copy(showUnstakeDialog = false)
    }

    fun unstake(amountLux: Long) {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(showUnstakeDialog = false)
            // In production: call GratiaNode.unstake(amountLux)
            delay(200)
            loadSettings()
        }
    }

    fun setLocationGranularity(granularity: LocationGranularity) {
        _uiState.value = _uiState.value.copy(locationGranularity = granularity)
    }

    fun setCameraHashEnabled(enabled: Boolean) {
        _uiState.value = _uiState.value.copy(cameraHashEnabled = enabled)
    }

    fun setMicrophoneFingerprintEnabled(enabled: Boolean) {
        _uiState.value = _uiState.value.copy(microphoneFingerprintEnabled = enabled)
    }

    fun setInheritanceEnabled(enabled: Boolean) {
        _uiState.value = _uiState.value.copy(inheritanceEnabled = enabled)
    }

    fun showBeneficiaryDialog() {
        _uiState.value = _uiState.value.copy(showBeneficiaryDialog = true)
    }

    fun hideBeneficiaryDialog() {
        _uiState.value = _uiState.value.copy(showBeneficiaryDialog = false)
    }

    fun setBeneficiaryAddress(address: String) {
        _uiState.value = _uiState.value.copy(
            beneficiaryAddress = address,
            showBeneficiaryDialog = false,
        )
    }

    private fun mockStakeInfo() = StakeInfo(
        nodeStakeLux = 500_000_000L,   // 500 GRAT
        overflowAmountLux = 0L,
        totalCommittedLux = 500_000_000L,
        stakedAtMillis = System.currentTimeMillis() - 86_400_000L * 15, // 15 days ago
        meetsMinimum = true,
    )
}

// ============================================================================
// Governance UI state
// ============================================================================

data class GovernanceUiState(
    val proposals: List<Proposal> = emptyList(),
    val polls: List<Poll> = emptyList(),
    val isLoading: Boolean = true,
    val selectedProposal: Proposal? = null,
    val selectedPoll: Poll? = null,
    val canCreateProposal: Boolean = false,
)

class GovernanceViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(GovernanceUiState())
    val uiState: StateFlow<GovernanceUiState> = _uiState.asStateFlow()

    init {
        loadGovernanceData()
    }

    fun loadGovernanceData() {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(isLoading = true)
            delay(300)
            _uiState.value = GovernanceUiState(
                proposals = mockProposals(),
                polls = mockPolls(),
                isLoading = false,
                canCreateProposal = false, // Requires 90+ days PoL
            )
        }
    }

    fun selectProposal(proposal: Proposal) {
        _uiState.value = _uiState.value.copy(selectedProposal = proposal)
    }

    fun clearSelectedProposal() {
        _uiState.value = _uiState.value.copy(selectedProposal = null)
    }

    fun voteOnProposal(proposalId: String, vote: String) {
        viewModelScope.launch {
            // In production: call governance contract
            delay(200)
            clearSelectedProposal()
            loadGovernanceData()
        }
    }

    fun selectPoll(poll: Poll) {
        _uiState.value = _uiState.value.copy(selectedPoll = poll)
    }

    fun clearSelectedPoll() {
        _uiState.value = _uiState.value.copy(selectedPoll = null)
    }

    fun voteOnPoll(pollId: String, optionIndex: Int) {
        viewModelScope.launch {
            // In production: call polling contract
            delay(200)
            clearSelectedPoll()
            loadGovernanceData()
        }
    }

    private fun mockProposals(): List<Proposal> {
        val now = System.currentTimeMillis()
        return listOf(
            Proposal(
                id = "prop-001",
                title = "Increase minimum stake to 200 GRAT",
                description = "This proposal suggests raising the minimum stake requirement from 100 GRAT to 200 GRAT to improve network security and reduce low-effort node participation.",
                status = "voting",
                votesFor = 1842,
                votesAgainst = 756,
                votesAbstain = 203,
                discussionEndMillis = now - 86_400_000L * 2,
                votingEndMillis = now + 86_400_000L * 5,
                submittedByAddress = "grat:aabb...ccdd",
            ),
            Proposal(
                id = "prop-002",
                title = "Add barometer to core sensor requirements",
                description = "Proposal to add barometer readings as a core requirement for Proof of Life, improving location verification fidelity.",
                status = "discussion",
                votesFor = 0,
                votesAgainst = 0,
                votesAbstain = 0,
                discussionEndMillis = now + 86_400_000L * 10,
                votingEndMillis = now + 86_400_000L * 17,
                submittedByAddress = "grat:eeff...1122",
            ),
            Proposal(
                id = "prop-003",
                title = "Reduce block time to 2 seconds",
                description = "Proposing a reduction of block time from 3-5 seconds to a fixed 2 seconds to improve transaction throughput.",
                status = "passed",
                votesFor = 3201,
                votesAgainst = 1150,
                votesAbstain = 412,
                discussionEndMillis = now - 86_400_000L * 30,
                votingEndMillis = now - 86_400_000L * 23,
                submittedByAddress = "grat:3344...5566",
            ),
        )
    }

    private fun mockPolls(): List<Poll> {
        val now = System.currentTimeMillis()
        return listOf(
            Poll(
                id = "poll-001",
                question = "What should the GRAT token icon look like?",
                options = listOf("Sun symbol", "Shield emblem", "Abstract G", "Fingerprint motif"),
                votes = listOf(456, 312, 789, 234),
                endMillis = now + 86_400_000L * 3,
                createdByAddress = "grat:7788...99aa",
                totalVoters = 1791,
            ),
            Poll(
                id = "poll-002",
                question = "Preferred geographic shard count for mainnet launch?",
                options = listOf("5 shards", "10 shards", "20 shards"),
                votes = listOf(892, 1456, 340),
                endMillis = now + 86_400_000L * 7,
                createdByAddress = "grat:bbcc...ddee",
                totalVoters = 2688,
            ),
        )
    }
}

// ============================================================================
// Utility functions shared across screens
// ============================================================================

/** Format Lux amount as GRAT with up to 6 decimal places, trimming trailing zeros. */
fun formatGrat(lux: Long): String {
    val whole = lux / 1_000_000L
    val fractional = lux % 1_000_000L
    return if (fractional == 0L) {
        "$whole"
    } else {
        val frac = "%06d".format(fractional).trimEnd('0')
        "$whole.$frac"
    }
}

/** Truncate a grat: address to show prefix and suffix. */
fun truncateAddress(address: String, prefixLen: Int = 10, suffixLen: Int = 6): String {
    if (address.length <= prefixLen + suffixLen + 3) return address
    return "${address.take(prefixLen)}...${address.takeLast(suffixLen)}"
}

/** Convert a mining state string to a human-readable label. */
fun miningStateLabel(state: String): String = when (state) {
    "mining" -> "Mining"
    "proof_of_life" -> "Proof of Life"
    "battery_low" -> "Battery Low"
    "throttled" -> "Throttled"
    "pending_activation" -> "Pending Activation"
    else -> state.replaceFirstChar { it.uppercase() }
}
