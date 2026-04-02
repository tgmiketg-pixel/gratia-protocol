package io.gratia.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import io.gratia.app.bridge.GratiaCoreManager
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

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
    val earnedThisSessionLux: Long = 0,
    val isConsensusActive: Boolean = false,
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
    val hasVotedOnProposal: Boolean = false,
)

data class Poll(
    val id: String,
    val question: String,
    val options: List<String>,
    val votes: List<Int>,
    val endMillis: Long,
    val createdByAddress: String,
    val totalVoters: Int,
    val hasVotedOnPoll: Boolean = false,
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
    /** Whether the restore wallet dialog is visible. */
    val showRestoreDialog: Boolean = false,
    /** Error from a failed restore attempt. */
    val restoreError: String? = null,
    /** Address after successful restore, null otherwise. */
    val restoredAddress: String? = null,
)

class WalletViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(WalletUiState())
    val uiState: StateFlow<WalletUiState> = _uiState.asStateFlow()

    init {
        loadWalletData()
        // WHY: Poll every 10 seconds so the wallet balance updates
        // as mining rewards are credited (1 GRAT/minute).
        startPolling()
    }

    private fun startPolling() {
        viewModelScope.launch(Dispatchers.IO) {
            while (true) {
                delay(10_000)
                refreshQuiet()
            }
        }
    }

    private var lastKnownTxCount: Int = 0

    private fun refreshQuiet() {
        try {
            val bridge = io.gratia.app.bridge.GratiaCoreManager
            val info = bridge.getWalletInfo()
            val txs = bridge.getTransactionHistory()
            val mappedTxs = txs.map { tx ->
                TransactionInfo(
                    hashHex = tx.hashHex,
                    direction = tx.direction,
                    counterparty = tx.counterparty,
                    amountLux = tx.amountLux,
                    timestampMillis = tx.timestampMillis,
                    status = tx.status,
                )
            }

            // WHY: Detect new incoming transactions by comparing tx count.
            // If a new "received" tx appeared, fire a push notification so
            // the user knows they got GRAT even if the app is in the background.
            if (lastKnownTxCount > 0 && mappedTxs.size > lastKnownTxCount) {
                val newTxs = mappedTxs.take(mappedTxs.size - lastKnownTxCount)
                for (tx in newTxs) {
                    if (tx.direction == "received") {
                        notifyIncomingTransfer(tx)
                    }
                }
            }
            lastKnownTxCount = mappedTxs.size

            _uiState.value = _uiState.value.copy(
                walletInfo = WalletInfo(
                    address = info.address,
                    balanceLux = info.balanceLux,
                    miningState = info.miningState,
                ),
                transactions = mappedTxs,
            )
        } catch (e: Exception) {
            android.util.Log.w("WalletVM", "Refresh failed: ${e.message}")
        }
    }

    private fun notifyIncomingTransfer(tx: TransactionInfo) {
        try {
            val context = io.gratia.app.GratiaApplication.appContext ?: return
            val amountGrat = "%.2f".format(tx.amountLux / 1_000_000.0)
            val notification = io.gratia.app.service.NotificationHelper.buildTransactionNotification(
                context, amountGrat, tx.counterparty ?: "Unknown",
            )
            val manager = context.getSystemService(android.content.Context.NOTIFICATION_SERVICE)
                as? android.app.NotificationManager
            // WHY: Use tx hash code as notification ID so each transfer gets its own notification.
            manager?.notify(tx.hashHex.hashCode(), notification)
        } catch (_: Exception) {}
    }

    fun loadWalletData() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isLoading = true, errorMessage = null)
            try {
                val bridge = io.gratia.app.bridge.GratiaCoreManager
                val info = bridge.getWalletInfo()
                val txs = bridge.getTransactionHistory()
                _uiState.value = _uiState.value.copy(
                    walletInfo = WalletInfo(
                        address = info.address,
                        balanceLux = info.balanceLux,
                        miningState = info.miningState,
                    ),
                    transactions = txs.map { tx ->
                        TransactionInfo(
                            hashHex = tx.hashHex,
                            direction = tx.direction,
                            counterparty = tx.counterparty,
                            amountLux = tx.amountLux,
                            timestampMillis = tx.timestampMillis,
                            status = tx.status,
                        )
                    },
                    isLoading = false,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    isLoading = false,
                    errorMessage = e.message,
                )
            }
        }
    }

    fun refresh() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isRefreshing = true)
            try {
                val bridge = io.gratia.app.bridge.GratiaCoreManager
                val info = bridge.getWalletInfo()
                val txs = bridge.getTransactionHistory()
                _uiState.value = _uiState.value.copy(
                    walletInfo = WalletInfo(
                        address = info.address,
                        balanceLux = info.balanceLux,
                        miningState = info.miningState,
                    ),
                    transactions = txs.map { tx ->
                        TransactionInfo(
                            hashHex = tx.hashHex,
                            direction = tx.direction,
                            counterparty = tx.counterparty,
                            amountLux = tx.amountLux,
                            timestampMillis = tx.timestampMillis,
                            status = tx.status,
                        )
                    },
                    isRefreshing = false,
                )
            } catch (_: Exception) {
                _uiState.value = _uiState.value.copy(isRefreshing = false)
            }
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
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(showSendDialog = false)
            try {
                val txHash = io.gratia.app.bridge.GratiaCoreManager.sendTransfer(toAddress, amountLux)
                android.util.Log.i("WalletViewModel", "Transfer sent: $txHash")
            } catch (e: Exception) {
                android.util.Log.e("WalletViewModel", "Transfer failed: ${e.message}")
                _uiState.value = _uiState.value.copy(
                    errorMessage = e.message,
                )
            }
            loadWalletData()
        }
    }

    // -- Restore wallet -------------------------------------------------------

    fun showRestoreDialog() {
        _uiState.value = _uiState.value.copy(
            showRestoreDialog = true,
            restoreError = null,
            restoredAddress = null,
        )
    }

    fun hideRestoreDialog() {
        _uiState.value = _uiState.value.copy(
            showRestoreDialog = false,
            restoreError = null,
        )
    }

    fun importSeedPhrase(seedHex: String) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val address = io.gratia.app.bridge.GratiaCoreManager.importSeedPhrase(seedHex)
                _uiState.value = _uiState.value.copy(
                    showRestoreDialog = false,
                    restoredAddress = address,
                    restoreError = null,
                )
                // WHY: Reload wallet data so balance and address update immediately.
                loadWalletData()
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    restoreError = e.message ?: "Import failed",
                )
            }
        }
    }

    fun clearRestoredAddress() {
        _uiState.value = _uiState.value.copy(restoredAddress = null)
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
    val stakeInfo: StakeInfo = StakeInfo(
        nodeStakeLux = 0L,
        overflowAmountLux = 0L,
        totalCommittedLux = 0L,
        stakedAtMillis = 0L,
        meetsMinimum = false,
    ),
    val isLoading: Boolean = true,
    val earningsToday: Long = 0L,
    val earningsThisWeek: Long = 0L,
    val earningsTotal: Long = 0L,
    val showStakeDialog: Boolean = false,
    val showUnstakeDialog: Boolean = false,
    val stakeError: String? = null,
    // Consensus & network info (folded from NetworkScreen)
    val peerCount: Int = 0,
    val blockHeight: Long = 0L,
    val currentSlot: Long = 0L,
    val isCommitteeMember: Boolean = false,
    val blocksProduced: Long = 0L,
    val syncStatus: String = "unknown",
    val isNetworkRunning: Boolean = false,
)

class MiningViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(MiningUiState())
    val uiState: StateFlow<MiningUiState> = _uiState.asStateFlow()

    init {
        loadMiningData()
        // WHY: Poll every 5 seconds so the PoL checklist updates in real-time
        // as sensor events are collected throughout the day.
        startPolling()
    }

    private fun startPolling() {
        viewModelScope.launch(Dispatchers.IO) {
            while (true) {
                delay(5000)
                loadMiningDataQuiet()
            }
        }
    }

    private fun loadMiningDataQuiet() {
        try {
            val bridge = io.gratia.app.bridge.GratiaCoreManager
            val mining = bridge.getMiningStatus()
            val pol = bridge.getProofOfLifeStatus()
            val stake = bridge.getStakeInfo()
            val walletBalance = try { bridge.getWalletInfo().balanceLux } catch (_: Exception) { 0L }
            val consensus = try { bridge.getConsensusStatus() } catch (_: Exception) { null }
            val consensusActive = consensus != null && consensus.state != "stopped"
            val network = try { bridge.getNetworkStatus() } catch (_: Exception) { null }
            val syncStatus = try { bridge.requestSync() } catch (_: Exception) { "unknown" }
            _uiState.value = _uiState.value.copy(
                miningStatus = MiningStatus(
                    state = mining.state,
                    batteryPercent = mining.batteryPercent,
                    isPluggedIn = mining.isPluggedIn,
                    currentDayPolValid = mining.currentDayPolValid,
                    presenceScore = mining.presenceScore,
                    earnedThisSessionLux = walletBalance,
                    isConsensusActive = consensusActive,
                ),
                polStatus = ProofOfLifeStatus(
                    isValidToday = pol.isValidToday,
                    consecutiveDays = pol.consecutiveDays,
                    isOnboarded = pol.isOnboarded,
                    parametersMet = pol.parametersMet,
                ),
                stakeInfo = StakeInfo(
                    nodeStakeLux = stake.nodeStakeLux,
                    overflowAmountLux = stake.overflowAmountLux,
                    totalCommittedLux = stake.totalCommittedLux,
                    stakedAtMillis = stake.stakedAtMillis,
                    meetsMinimum = stake.meetsMinimum,
                ),
                peerCount = network?.peerCount ?: 0,
                blockHeight = consensus?.currentHeight ?: 0L,
                currentSlot = consensus?.currentSlot ?: 0L,
                isCommitteeMember = consensus?.isCommitteeMember ?: false,
                blocksProduced = consensus?.blocksProduced ?: 0L,
                syncStatus = syncStatus,
                isNetworkRunning = network?.isRunning ?: false,
            )
        } catch (e: Exception) {
            android.util.Log.w("MiningVM", "Quiet refresh failed: ${e.message}")
        }
    }

    fun loadMiningData() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isLoading = true)
            try {
                val bridge = io.gratia.app.bridge.GratiaCoreManager
                val mining = bridge.getMiningStatus()
                val pol = bridge.getProofOfLifeStatus()
                val stake = bridge.getStakeInfo()
                val walletBalance = try { bridge.getWalletInfo().balanceLux } catch (_: Exception) { 0L }
                val consensus = try { bridge.getConsensusStatus() } catch (_: Exception) { null }
                val consensusActive = consensus != null && consensus.state != "stopped"
                val network = try { bridge.getNetworkStatus() } catch (_: Exception) { null }
                val syncStatus = try { bridge.requestSync() } catch (_: Exception) { "unknown" }
                _uiState.value = MiningUiState(
                    miningStatus = MiningStatus(
                        state = mining.state,
                        batteryPercent = mining.batteryPercent,
                        isPluggedIn = mining.isPluggedIn,
                        currentDayPolValid = mining.currentDayPolValid,
                        presenceScore = mining.presenceScore,
                        earnedThisSessionLux = walletBalance,
                        isConsensusActive = consensusActive,
                    ),
                    polStatus = ProofOfLifeStatus(
                        isValidToday = pol.isValidToday,
                        consecutiveDays = pol.consecutiveDays,
                        isOnboarded = pol.isOnboarded,
                        parametersMet = pol.parametersMet,
                    ),
                    stakeInfo = StakeInfo(
                        nodeStakeLux = stake.nodeStakeLux,
                        overflowAmountLux = stake.overflowAmountLux,
                        totalCommittedLux = stake.totalCommittedLux,
                        stakedAtMillis = stake.stakedAtMillis,
                        meetsMinimum = stake.meetsMinimum,
                    ),
                    isLoading = false,
                    earningsToday = 0L,
                    earningsThisWeek = 0L,
                    earningsTotal = 0L,
                    peerCount = network?.peerCount ?: 0,
                    blockHeight = consensus?.currentHeight ?: 0L,
                    currentSlot = consensus?.currentSlot ?: 0L,
                    isCommitteeMember = consensus?.isCommitteeMember ?: false,
                    blocksProduced = consensus?.blocksProduced ?: 0L,
                    syncStatus = syncStatus,
                    isNetworkRunning = network?.isRunning ?: false,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(isLoading = false)
            }
        }
    }

    fun startMining() {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val result = io.gratia.app.bridge.GratiaCoreManager.startMining()
                _uiState.value = _uiState.value.copy(
                    miningStatus = MiningStatus(
                        state = result.state,
                        batteryPercent = result.batteryPercent,
                        isPluggedIn = result.isPluggedIn,
                        currentDayPolValid = result.currentDayPolValid,
                        presenceScore = result.presenceScore,
                    )
                )
            } catch (_: Exception) {}
        }
    }

    fun stopMining() {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val result = io.gratia.app.bridge.GratiaCoreManager.stopMining()
                _uiState.value = _uiState.value.copy(
                    miningStatus = MiningStatus(
                        state = result.state,
                        batteryPercent = result.batteryPercent,
                        isPluggedIn = result.isPluggedIn,
                        currentDayPolValid = result.currentDayPolValid,
                        presenceScore = result.presenceScore,
                    )
                )
            } catch (_: Exception) {}
        }
    }

    // -- Staking ---------------------------------------------------------------

    fun showStakeDialog() {
        _uiState.value = _uiState.value.copy(showStakeDialog = true, stakeError = null)
    }

    fun hideStakeDialog() {
        _uiState.value = _uiState.value.copy(showStakeDialog = false, stakeError = null)
    }

    fun showUnstakeDialog() {
        _uiState.value = _uiState.value.copy(showUnstakeDialog = true, stakeError = null)
    }

    fun hideUnstakeDialog() {
        _uiState.value = _uiState.value.copy(showUnstakeDialog = false, stakeError = null)
    }

    fun stake(amountLux: Long) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val txHash = io.gratia.app.bridge.GratiaCoreManager.stake(amountLux)
                android.util.Log.i("MiningViewModel", "Stake tx: $txHash")
                _uiState.value = _uiState.value.copy(showStakeDialog = false, stakeError = null)
                // WHY: Reload stake info immediately so the UI reflects the new stake
                // without waiting for the next 5-second poll cycle.
                loadMiningDataQuiet()
            } catch (e: Exception) {
                android.util.Log.e("MiningViewModel", "Stake failed: ${e.message}")
                _uiState.value = _uiState.value.copy(stakeError = e.message)
            }
        }
    }

    fun unstake(amountLux: Long) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val txHash = io.gratia.app.bridge.GratiaCoreManager.unstake(amountLux)
                android.util.Log.i("MiningViewModel", "Unstake tx: $txHash")
                _uiState.value = _uiState.value.copy(showUnstakeDialog = false, stakeError = null)
                // WHY: Same as stake — immediate refresh for responsiveness.
                loadMiningDataQuiet()
            } catch (e: Exception) {
                android.util.Log.e("MiningViewModel", "Unstake failed: ${e.message}")
                _uiState.value = _uiState.value.copy(stakeError = e.message)
            }
        }
    }
}

// ============================================================================
// Network UI state
// ============================================================================

data class MeshStatus(
    val enabled: Boolean = false,
    val bluetoothActive: Boolean = false,
    val wifiDirectActive: Boolean = false,
    val meshPeerCount: Int = 0,
    val bridgePeerCount: Int = 0,
    val pendingRelayCount: Int = 0,
)

data class NetworkUiState(
    // WHY: Default to true/active since GratiaApplication auto-starts
    // network and consensus. The UI should show "active" immediately
    // instead of flashing "stopped" for 1-2 seconds while the async
    // fetch completes.
    val isNetworkRunning: Boolean = true,
    val peerCount: Int = 0,
    val listenAddress: String? = null,
    val consensusState: String = "active",
    val currentSlot: Long = 0,
    val currentHeight: Long = 0,
    val isCommitteeMember: Boolean = false,
    val blocksProduced: Long = 0,
    val meshStatus: MeshStatus = MeshStatus(),
    val isMeshLoading: Boolean = false,
    val recentEvents: List<String> = emptyList(),
    val isLoading: Boolean = false,
    val errorMessage: String? = null,
)

class NetworkViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(NetworkUiState())
    val uiState: StateFlow<NetworkUiState> = _uiState.asStateFlow()

    private var pollingActive = false

    init {
        // WHY: The network is auto-started by GratiaApplication on launch.
        // Always assume it's running so the Connect Peer card is visible.
        // If getNetworkStatus confirms running, update peer count too.
        _uiState.value = _uiState.value.copy(isNetworkRunning = true)
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val status = io.gratia.app.bridge.GratiaCoreManager.getNetworkStatus()
                _uiState.value = _uiState.value.copy(
                    isNetworkRunning = true, // Always true — auto-started by Application
                    peerCount = status.peerCount,
                    listenAddress = status.listenAddress,
                )
                startPolling()
            } catch (e: Exception) {
                android.util.Log.w("NetworkViewModel", "getNetworkStatus failed: ${e.message}")
                // Still keep isNetworkRunning=true so Connect card is visible
                startPolling()
            }
        }
    }

    /** Called from the composable LaunchedEffect to ensure periodic UI refresh. */
    fun refresh() {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val status = io.gratia.app.bridge.GratiaCoreManager.getNetworkStatus()
                _uiState.value = _uiState.value.copy(
                    isNetworkRunning = status.isRunning,
                    peerCount = status.peerCount,
                    listenAddress = status.listenAddress,
                )
                val conStatus = io.gratia.app.bridge.GratiaCoreManager.getConsensusStatus()
                _uiState.value = _uiState.value.copy(
                    consensusState = conStatus.state,
                    currentSlot = conStatus.currentSlot,
                    currentHeight = conStatus.currentHeight,
                    isCommitteeMember = conStatus.isCommitteeMember,
                    blocksProduced = conStatus.blocksProduced,
                )
            } catch (_: Exception) {}
        }
    }

    fun startNetwork() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isLoading = true, errorMessage = null)
            try {
                val status = io.gratia.app.bridge.GratiaCoreManager.startNetwork()
                _uiState.value = _uiState.value.copy(
                    isNetworkRunning = status.isRunning,
                    peerCount = status.peerCount,
                    listenAddress = status.listenAddress,
                    isLoading = false,
                )
                startPolling()
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    isLoading = false,
                    errorMessage = e.message,
                )
            }
        }
    }

    fun stopNetwork() {
        viewModelScope.launch(Dispatchers.IO) {
            pollingActive = false
            try {
                // WHY: Stop mesh first — it depends on the network layer for bridge relay.
                io.gratia.app.bridge.GratiaCoreManager.stopMesh()
            } catch (_: Exception) {}
            try {
                io.gratia.app.bridge.GratiaCoreManager.stopNetwork()
                io.gratia.app.bridge.GratiaCoreManager.stopConsensus()
            } catch (_: Exception) {}
            _uiState.value = NetworkUiState()
        }
    }

    fun startConsensus() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(errorMessage = null)
            try {
                val status = io.gratia.app.bridge.GratiaCoreManager.startConsensus()
                _uiState.value = _uiState.value.copy(
                    consensusState = status.state,
                    currentSlot = status.currentSlot,
                    currentHeight = status.currentHeight,
                    isCommitteeMember = status.isCommitteeMember,
                    blocksProduced = status.blocksProduced,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    errorMessage = e.message,
                )
            }
        }
    }

    fun stopConsensus() {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                io.gratia.app.bridge.GratiaCoreManager.stopConsensus()
                _uiState.value = _uiState.value.copy(
                    consensusState = "stopped",
                    currentSlot = 0,
                    currentHeight = 0,
                    isCommitteeMember = false,
                )
            } catch (_: Exception) {}
        }
    }

    fun connectPeer(address: String) {
        android.util.Log.i("NetworkViewModel", "connectPeer called with: '$address'")
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(errorMessage = null)
            try {
                android.util.Log.i("NetworkViewModel", "Calling FFI connectPeer...")
                io.gratia.app.bridge.GratiaCoreManager.connectPeer(address)
                android.util.Log.i("NetworkViewModel", "FFI connectPeer succeeded")
                addEvent("Dialing $address")
            } catch (e: Exception) {
                android.util.Log.e("NetworkViewModel", "FFI connectPeer failed: ${e.message}", e)
                _uiState.value = _uiState.value.copy(
                    errorMessage = e.message,
                )
            }
        }
    }

    // -- Mesh transport controls ------------------------------------------------

    fun startMesh() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isMeshLoading = true, errorMessage = null)
            try {
                io.gratia.app.bridge.GratiaCoreManager.startMesh()
                val status = io.gratia.app.bridge.GratiaCoreManager.getMeshStatus()
                _uiState.value = _uiState.value.copy(
                    meshStatus = MeshStatus(
                        enabled = status.enabled,
                        bluetoothActive = status.bluetoothActive,
                        wifiDirectActive = status.wifiDirectActive,
                        meshPeerCount = status.meshPeerCount,
                        bridgePeerCount = status.bridgePeerCount,
                        pendingRelayCount = status.pendingRelayCount,
                    ),
                    isMeshLoading = false,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    isMeshLoading = false,
                    errorMessage = e.message,
                )
            }
        }
    }

    fun stopMesh() {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                io.gratia.app.bridge.GratiaCoreManager.stopMesh()
                _uiState.value = _uiState.value.copy(meshStatus = MeshStatus())
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(errorMessage = e.message)
            }
        }
    }

    fun clearError() {
        _uiState.value = _uiState.value.copy(errorMessage = null)
    }

    private fun startPolling() {
        if (pollingActive) return
        pollingActive = true
        viewModelScope.launch(Dispatchers.IO) {
            // WHY: Poll every 2 seconds — fast enough to show live block production
            // (4-second slots) without excessive CPU usage on mobile.
            while (pollingActive) {
                delay(2000)
                pollStatus()
            }
        }
    }

    private suspend fun pollStatus() {
        try {
            // Poll network status
            val netStatus = io.gratia.app.bridge.GratiaCoreManager.getNetworkStatus()
            _uiState.value = _uiState.value.copy(
                isNetworkRunning = netStatus.isRunning,
                peerCount = netStatus.peerCount,
                listenAddress = netStatus.listenAddress,
            )

            // Poll consensus status
            val conStatus = io.gratia.app.bridge.GratiaCoreManager.getConsensusStatus()
            _uiState.value = _uiState.value.copy(
                consensusState = conStatus.state,
                currentSlot = conStatus.currentSlot,
                currentHeight = conStatus.currentHeight,
                isCommitteeMember = conStatus.isCommitteeMember,
                blocksProduced = conStatus.blocksProduced,
            )

            // Poll mesh status (if enabled)
            try {
                val meshStatus = io.gratia.app.bridge.GratiaCoreManager.getMeshStatus()
                _uiState.value = _uiState.value.copy(
                    meshStatus = MeshStatus(
                        enabled = meshStatus.enabled,
                        bluetoothActive = meshStatus.bluetoothActive,
                        wifiDirectActive = meshStatus.wifiDirectActive,
                        meshPeerCount = meshStatus.meshPeerCount,
                        bridgePeerCount = meshStatus.bridgePeerCount,
                        pendingRelayCount = meshStatus.pendingRelayCount,
                    ),
                )
            } catch (_: Exception) {}

            // Poll network events
            val events = io.gratia.app.bridge.GratiaCoreManager.pollNetworkEvents()
            for (event in events) {
                val msg = when (event) {
                    is io.gratia.app.bridge.NetworkEvent.PeerConnected -> {
                        // WHY: Update peer count immediately on connect for responsive UI
                        _uiState.value = _uiState.value.copy(
                            peerCount = _uiState.value.peerCount + 1,
                        )
                        "Peer connected: ${event.peerId.take(12)}..."
                    }
                    is io.gratia.app.bridge.NetworkEvent.PeerDisconnected -> {
                        _uiState.value = _uiState.value.copy(
                            peerCount = (_uiState.value.peerCount - 1).coerceAtLeast(0),
                        )
                        "Peer disconnected: ${event.peerId.take(12)}..."
                    }
                    is io.gratia.app.bridge.NetworkEvent.BlockReceived ->
                        "Block #${event.height} from ${event.producer.take(8)}..."
                    is io.gratia.app.bridge.NetworkEvent.TransactionReceived ->
                        "Tx ${event.hashHex.take(8)}... received"
                    is io.gratia.app.bridge.NetworkEvent.LuxPostReceived ->
                        "Lux post from ${event.author.take(14)}..."
                }
                addEvent(msg)
            }
        } catch (_: Exception) {}
    }

    private fun addEvent(message: String) {
        val events = _uiState.value.recentEvents.toMutableList()
        events.add(0, message)
        // WHY: Keep only last 50 events to bound memory usage.
        if (events.size > 50) events.removeAt(events.size - 1)
        _uiState.value = _uiState.value.copy(recentEvents = events)
    }
}

// ============================================================================
// Settings UI state
// ============================================================================

data class ShardInfoUi(
    val shardId: Int = 0,
    val shardCount: Int = 1,
    val localValidators: Int = 0,
    val crossShardValidators: Int = 0,
    val shardHeight: Long = 0,
    val isShardingActive: Boolean = false,
    val crossShardQueueSize: Int = 0,
)

data class VmInfoUi(
    val runtimeType: String = "wasmer",
    val contractsLoaded: Int = 0,
    val totalGasUsed: Long = 0,
    val memoryWired: Boolean = false,
)

data class SettingsUiState(
    val stakeInfo: StakeInfo? = null,
    val balanceLux: Long = 0,
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
    /** Exported seed phrase hex string, null when not yet exported. */
    val exportedSeedPhrase: String? = null,
    val shardInfo: ShardInfoUi = ShardInfoUi(),
    val vmInfo: VmInfoUi = VmInfoUi(),
    /** Whether the restore wallet dialog is visible. */
    val showRestoreDialog: Boolean = false,
    /** Result address after a successful import, null otherwise. */
    val restoredAddress: String? = null,
    /** Error message from a failed import attempt. */
    val restoreError: String? = null,
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
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isLoading = true)

            // Fetch real data from the Rust core where available
            val stakeInfo = try {
                val s = GratiaCoreManager.getStakeInfo()
                StakeInfo(
                    nodeStakeLux = s.nodeStakeLux,
                    overflowAmountLux = s.overflowAmountLux,
                    totalCommittedLux = s.totalCommittedLux,
                    stakedAtMillis = s.stakedAtMillis,
                    meetsMinimum = s.meetsMinimum,
                )
            } catch (_: Exception) { mockStakeInfo() }

            val balanceLux = try {
                GratiaCoreManager.getWalletInfo().balanceLux
            } catch (_: Exception) { 0L }

            val nodeId = try {
                GratiaCoreManager.getWalletInfo().address
            } catch (_: Exception) { "grat:a1b2c3d4e5f6a1b2" }

            val participationDays = try {
                GratiaCoreManager.getProofOfLifeStatus().consecutiveDays
            } catch (_: Exception) { 0L }

            val shardInfoUi = try {
                val si = GratiaCoreManager.getShardInfo()
                val queueSize = try {
                    GratiaCoreManager.getCrossShardQueueSize()
                } catch (_: Exception) { 0 }
                ShardInfoUi(
                    shardId = si.shardId,
                    shardCount = si.shardCount,
                    localValidators = si.localValidators,
                    crossShardValidators = si.crossShardValidators,
                    shardHeight = si.shardHeight,
                    isShardingActive = si.isShardingActive,
                    crossShardQueueSize = queueSize,
                )
            } catch (_: Exception) { ShardInfoUi() }

            val vmInfoUi = try {
                val vi = GratiaCoreManager.getVmInfo()
                VmInfoUi(
                    runtimeType = vi.runtimeType,
                    contractsLoaded = vi.contractsLoaded,
                    totalGasUsed = vi.totalGasUsed,
                    memoryWired = vi.memoryWired,
                )
            } catch (_: Exception) { VmInfoUi() }

            _uiState.value = _uiState.value.copy(
                stakeInfo = stakeInfo,
                balanceLux = balanceLux,
                isLoading = false,
                nodeId = nodeId,
                appVersion = "0.3.0-alpha",
                participationDays = participationDays,
                shardInfo = shardInfoUi,
                vmInfo = vmInfoUi,
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
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val hexPhrase = GratiaCoreManager.exportSeedPhrase()
                _uiState.value = _uiState.value.copy(
                    showExportSeedConfirmation = false,
                    exportedSeedPhrase = hexPhrase,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    showExportSeedConfirmation = false,
                    exportedSeedPhrase = null,
                )
            }
        }
    }

    fun clearExportedSeedPhrase() {
        _uiState.value = _uiState.value.copy(exportedSeedPhrase = null)
    }

    fun showRestoreDialog() {
        _uiState.value = _uiState.value.copy(
            showRestoreDialog = true,
            restoredAddress = null,
            restoreError = null,
        )
    }

    fun hideRestoreDialog() {
        _uiState.value = _uiState.value.copy(
            showRestoreDialog = false,
            restoredAddress = null,
            restoreError = null,
        )
    }

    fun importSeedPhrase(seedHex: String) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val address = GratiaCoreManager.importSeedPhrase(seedHex)
                _uiState.value = _uiState.value.copy(
                    showRestoreDialog = false,
                    restoredAddress = address,
                    restoreError = null,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    restoreError = e.message ?: "Import failed",
                )
            }
        }
    }

    fun clearRestoredAddress() {
        _uiState.value = _uiState.value.copy(restoredAddress = null)
    }

    fun showStakeDialog() {
        _uiState.value = _uiState.value.copy(showStakeDialog = true)
    }

    fun hideStakeDialog() {
        _uiState.value = _uiState.value.copy(showStakeDialog = false)
    }

    fun stake(amountLux: Long) {
        viewModelScope.launch(Dispatchers.IO) {
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
        viewModelScope.launch(Dispatchers.IO) {
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
    // WHY: participationDays lets the create dialog show how many more days are needed
    // to meet the 90-day Proof of Life requirement for proposal submission.
    val participationDays: Long = 0,
    // WHY: walletBalanceLux lets the poll create dialog show cost vs. available balance.
    val walletBalanceLux: Long = 0,
)

class GovernanceViewModel : ViewModel() {

    private val _uiState = MutableStateFlow(GovernanceUiState())
    val uiState: StateFlow<GovernanceUiState> = _uiState.asStateFlow()

    init {
        loadGovernanceData()
    }

    fun loadGovernanceData() {
        viewModelScope.launch(Dispatchers.IO) {
            _uiState.value = _uiState.value.copy(isLoading = true)

            // WHY: Check PoL consecutive days for proposal eligibility (90+ days required).
            // In debug bypass mode, the Rust side allows it regardless.
            var participationDays = 0L
            val canCreate = try {
                val pol = GratiaCoreManager.getProofOfLifeStatus()
                participationDays = pol.consecutiveDays
                pol.consecutiveDays >= 90
            } catch (_: Exception) {
                participationDays = 999 // Assume qualified during development
                true // Default true during development
            }

            // WHY: Wallet balance is shown in the poll creation dialog so users
            // know whether they can afford the poll creation burn cost.
            val walletBalance = try {
                GratiaCoreManager.getWalletInfo().balanceLux
            } catch (_: Exception) {
                0L
            }

            // Fetch real proposals and polls from the Rust governance engine
            val proposals = try {
                GratiaCoreManager.getProposals().map { p ->
                    Proposal(
                        id = p.idHex,
                        title = p.title,
                        description = p.description,
                        status = p.status,
                        votesFor = p.votesYes.toInt(),
                        votesAgainst = p.votesNo.toInt(),
                        votesAbstain = p.votesAbstain.toInt(),
                        discussionEndMillis = p.discussionEndMillis,
                        votingEndMillis = p.votingEndMillis,
                        submittedByAddress = p.submittedBy,
                    )
                }
            } catch (_: Exception) { emptyList() }

            val polls = try {
                GratiaCoreManager.getPolls().map { p ->
                    Poll(
                        id = p.idHex,
                        question = p.question,
                        options = p.options,
                        votes = p.votes.map { it.toInt() },
                        endMillis = p.endMillis,
                        createdByAddress = p.createdBy,
                        totalVoters = p.totalVoters.toInt(),
                    )
                }
            } catch (_: Exception) { emptyList() }

            _uiState.value = GovernanceUiState(
                proposals = proposals,
                polls = polls,
                isLoading = false,
                canCreateProposal = canCreate,
                participationDays = participationDays,
                walletBalanceLux = walletBalance,
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
        viewModelScope.launch(Dispatchers.IO) {
            try {
                GratiaCoreManager.voteOnProposal(proposalId, vote)
                android.util.Log.i("GovernanceViewModel", "Vote '$vote' on proposal $proposalId")
            } catch (e: Exception) {
                android.util.Log.e("GovernanceViewModel", "Vote failed: ${e.message}")
            }
            // Refresh from Rust to get updated vote counts
            _uiState.value = _uiState.value.copy(selectedProposal = null)
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
        viewModelScope.launch(Dispatchers.IO) {
            try {
                GratiaCoreManager.voteOnPoll(pollId, optionIndex)
                android.util.Log.i("GovernanceViewModel", "Vote option $optionIndex on poll $pollId")
            } catch (e: Exception) {
                android.util.Log.e("GovernanceViewModel", "Poll vote failed: ${e.message}")
            }
            _uiState.value = _uiState.value.copy(selectedPoll = null)
            loadGovernanceData()
        }
    }

    fun createPoll(question: String, options: List<String>) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val pollId = GratiaCoreManager.createPoll(question, options)
                android.util.Log.i("GovernanceViewModel", "Poll created: $question (id=$pollId)")
            } catch (e: Exception) {
                android.util.Log.e("GovernanceViewModel", "Create poll failed: ${e.message}")
            }
            loadGovernanceData()
        }
    }

    fun createProposal(title: String, description: String) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val proposalId = GratiaCoreManager.submitProposal(title, description)
                android.util.Log.i("GovernanceViewModel", "Proposal created: $title (id=$proposalId)")
            } catch (e: Exception) {
                android.util.Log.e("GovernanceViewModel", "Create proposal failed: ${e.message}")
            }
            loadGovernanceData()
        }
    }
}

// ============================================================================
// Utility functions shared across screens
// ============================================================================

/** Format Lux amount as GRAT with 2 decimal places (e.g. "2,324.72 GRAT"). */
fun formatGrat(lux: Long): String {
    val whole = lux / 1_000_000L
    val fractional = (lux % 1_000_000L) / 10_000L // 2 decimal places
    val wholeStr = "%,d".format(whole)
    return "$wholeStr.%02d".format(fractional)
}

/** Format Lux amount as GRAT with up to 6 decimal places, trimming trailing zeros. */
fun formatGratPrecise(lux: Long): String {
    val whole = lux / 1_000_000L
    val fractional = lux % 1_000_000L
    return if (fractional == 0L) {
        "%,d".format(whole)
    } else {
        val frac = "%06d".format(fractional).trimEnd('0')
        "%,d".format(whole) + ".$frac"
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
