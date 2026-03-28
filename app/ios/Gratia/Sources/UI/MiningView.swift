import SwiftUI

// ============================================================================
// MiningView — Mining status, start/stop, battery, PoL, staking
//
// Mirrors Android's MiningScreen.kt exactly:
// - Mining state indicator with pulse animation
// - Battery and power status
// - Proof of Life parameter checklist
// - Presence Score display
// - Mining earnings summary
// - Staking controls
// ============================================================================

// MARK: - View Model

@MainActor
final class MiningViewModel: ObservableObject {
    @Published var miningStatus: MiningStatus?
    @Published var polStatus: ProofOfLifeStatus?
    @Published var stakeInfo = StakeInfo(nodeStakeLux: 0, overflowAmountLux: 0, totalCommittedLux: 0, stakedAtMillis: 0, meetsMinimum: false)
    @Published var earningsToday: Int64 = 0
    @Published var earningsThisWeek: Int64 = 0
    @Published var earningsTotal: Int64 = 0
    @Published var isLoading = true
    @Published var showStakeSheet = false
    @Published var showUnstakeSheet = false
    @Published var stakeError: String?

    private var pollingTask: Task<Void, Never>?

    init() {
        loadData()
        startPolling()
    }

    deinit {
        pollingTask?.cancel()
    }

    private func startPolling() {
        pollingTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000) // 5 seconds
                await refreshQuiet()
            }
        }
    }

    private func refreshQuiet() async {
        guard GratiaCoreManager.shared.isInitialized else { return }
        do {
            miningStatus = try GratiaCoreManager.shared.getMiningStatus()
            polStatus = try GratiaCoreManager.shared.getProofOfLifeStatus()
            stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
        } catch {}
    }

    func loadData() {
        isLoading = true
        Task {
            guard GratiaCoreManager.shared.isInitialized else {
                isLoading = false
                return
            }
            do {
                miningStatus = try GratiaCoreManager.shared.getMiningStatus()
                polStatus = try GratiaCoreManager.shared.getProofOfLifeStatus()
                stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
            } catch {}
            isLoading = false
        }
    }

    func startMining() {
        Task {
            do {
                miningStatus = try GratiaCoreManager.shared.startMining()
            } catch {}
        }
    }

    func stopMining() {
        Task {
            do {
                miningStatus = try GratiaCoreManager.shared.stopMining()
            } catch {}
        }
    }

    func stake(_ amountLux: Int64) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.stake(amountLux: amountLux)
                stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
                showStakeSheet = false
                stakeError = nil
            } catch {
                stakeError = error.localizedDescription
            }
        }
    }

    func unstake(_ amountLux: Int64) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.unstake(amountLux: amountLux)
                stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
                showUnstakeSheet = false
                stakeError = nil
            } catch {
                stakeError = error.localizedDescription
            }
        }
    }
}

// MARK: - Mining View

struct MiningView: View {
    @StateObject private var viewModel = MiningViewModel()

    var body: some View {
        NavigationStack {
            Group {
                if viewModel.isLoading {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    miningContent
                }
            }
            .navigationTitle("Mining")
            .sheet(isPresented: $viewModel.showStakeSheet) {
                StakeAmountSheet(
                    title: "Stake GRAT",
                    actionLabel: "Stake",
                    errorMessage: viewModel.stakeError
                ) { amountGrat in
                    // WHY: Convert whole GRAT to Lux (1 GRAT = 1,000,000 Lux)
                    // because the FFI bridge operates in the smallest unit.
                    viewModel.stake(amountGrat * 1_000_000)
                }
            }
            .sheet(isPresented: $viewModel.showUnstakeSheet) {
                StakeAmountSheet(
                    title: "Unstake GRAT",
                    actionLabel: "Unstake",
                    errorMessage: viewModel.stakeError
                ) { amountGrat in
                    viewModel.unstake(amountGrat * 1_000_000)
                }
            }
        }
    }

    @ViewBuilder
    private var miningContent: some View {
        if let mining = viewModel.miningStatus {
            List {
                // Mining state card
                Section {
                    MiningStateCard(
                        status: mining,
                        onStart: { viewModel.startMining() },
                        onStop: { viewModel.stopMining() }
                    )
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)

                // Battery status
                Section("Power Status") {
                    BatteryStatusRow(status: mining)
                }

                // Proof of Life
                if let pol = viewModel.polStatus {
                    Section("Proof of Life") {
                        ProofOfLifeSection(polStatus: pol)
                    }
                }

                // Presence Score
                if mining.presenceScore > 0 {
                    Section("Presence Score") {
                        PresenceScoreRow(score: mining.presenceScore)
                    }
                }

                // Earnings
                Section("Mining Earnings") {
                    EarningsRow(label: "Today", amountLux: viewModel.earningsToday)
                    EarningsRow(label: "This Week", amountLux: viewModel.earningsThisWeek)
                    EarningsRow(label: "Total", amountLux: viewModel.earningsTotal)
                }

                // Staking
                Section("Staking") {
                    StakingSection(
                        stakeInfo: viewModel.stakeInfo,
                        onStake: { viewModel.showStakeSheet = true },
                        onUnstake: { viewModel.showUnstakeSheet = true }
                    )
                }
            }
            .listStyle(.insetGrouped)
        }
    }
}

// MARK: - Mining State Card

private struct MiningStateCard: View {
    let status: MiningStatus
    let onStart: () -> Void
    let onStop: () -> Void

    private var stateColor: Color {
        switch status.state {
        case "mining": return .signalGreen
        case "proof_of_life": return .charcoalNavy
        case "battery_low": return .amberGold
        case "throttled": return .darkAmber
        case "pending_activation": return .agedGold
        default: return .gray
        }
    }

    private var isMining: Bool { status.state == "mining" }

    private var stateLabel: String {
        switch status.state {
        case "mining": return "Mining Active"
        case "proof_of_life": return "Proof of Life"
        case "battery_low": return "Battery Low"
        case "throttled": return "Throttled"
        case "pending_activation": return "Pending"
        default: return status.state.capitalized
        }
    }

    private var stateDescription: String {
        switch status.state {
        case "mining": return "Earning GRAT at flat rate"
        case "proof_of_life": return "Passively collecting sensor data"
        case "battery_low": return "Battery at \(status.batteryPercent)% -- need 80%+"
        case "throttled": return "CPU temperature too high -- workload reduced"
        case "pending_activation": return "Waiting for mining conditions to be met"
        default: return ""
        }
    }

    var body: some View {
        VStack(spacing: 12) {
            // Animated indicator
            ZStack {
                if isMining {
                    MiningPulseAnimation(color: stateColor)
                }
                Circle()
                    .fill(stateColor)
                    .frame(width: 48, height: 48)
            }
            .frame(width: 80, height: 80)

            Text(stateLabel)
                .font(.title2)
                .fontWeight(.bold)
                .foregroundStyle(stateColor)

            Text(stateDescription)
                .font(.subheadline)
                .foregroundStyle(.secondary)

            // Start / Stop button
            if isMining {
                Button(role: .destructive) {
                    onStop()
                } label: {
                    Label("Stop Mining", systemImage: "stop.fill")
                }
                .buttonStyle(.bordered)
            } else if (status.state == "proof_of_life" || status.state == "pending_activation")
                        && status.isPluggedIn && status.batteryPercent >= 80 && status.currentDayPolValid {
                Button {
                    onStart()
                } label: {
                    Label("Start Mining", systemImage: "play.fill")
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(20)
        .background(stateColor.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

/// Pulsing ring animation behind the mining state circle.
///
/// WHY: Matches Android's MiningPulseAnimation using SwiftUI animation primitives.
/// The repeating scale+fade gives a "heartbeat" visual cue that mining is active.
private struct MiningPulseAnimation: View {
    let color: Color
    @State private var animate = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 80, height: 80)
            .scaleEffect(animate ? 1.8 : 1.0)
            .opacity(animate ? 0.0 : 0.4)
            .animation(
                .easeOut(duration: 1.5).repeatForever(autoreverses: false),
                value: animate
            )
            .onAppear { animate = true }
    }
}

// MARK: - Battery Status

private struct BatteryStatusRow: View {
    let status: MiningStatus

    private var batteryColor: Color {
        if status.batteryPercent >= 80 { return .signalGreen }
        if status.batteryPercent >= 50 { return .amberGold }
        return .alertRed
    }

    var body: some View {
        VStack(spacing: 12) {
            HStack {
                Image(systemName: status.isPluggedIn ? "battery.100.bolt" : "battery.100")
                    .foregroundStyle(batteryColor)

                VStack(alignment: .leading) {
                    HStack {
                        Text("Battery")
                        Spacer()
                        Text("\(status.batteryPercent)%")
                            .fontWeight(.semibold)
                    }
                    ProgressView(value: Double(status.batteryPercent), total: 100)
                        .tint(batteryColor)
                }
            }

            HStack {
                Image(systemName: status.isPluggedIn ? "powerplug.fill" : "powerplug")
                    .foregroundStyle(status.isPluggedIn ? .signalGreen : .secondary)

                Text(status.isPluggedIn ? "Connected to power" : "Not connected to power")
                    .font(.subheadline)

                Spacer()
            }
        }
    }
}

// MARK: - Proof of Life Section

/// All PoL parameter keys in display order, matching the FFI parameter names.
private let allPolParameters: [(key: String, label: String)] = [
    ("unlocks", "10+ unlock events"),
    ("unlock_spread", "Unlocks spread across 6+ hours"),
    ("interactions", "Screen interaction sessions"),
    ("orientation", "Orientation change detected"),
    ("motion", "Human-consistent motion"),
    ("gps", "GPS fix obtained"),
    ("network", "Wi-Fi or Bluetooth connectivity"),
    ("bt_variation", "Bluetooth environment variation"),
    ("charge_event", "Charge cycle event"),
]

private struct ProofOfLifeSection: View {
    let polStatus: ProofOfLifeStatus

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(polStatus.isValidToday ? "Valid" : "Incomplete")
                    .font(.subheadline)
                    .fontWeight(.semibold)
                    .foregroundStyle(polStatus.isValidToday ? .signalGreen : .amberGold)

                Spacer()

                if polStatus.consecutiveDays > 0 {
                    Text("\(polStatus.consecutiveDays) consecutive days")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            ForEach(allPolParameters, id: \.key) { param in
                let met = polStatus.parametersMet.contains(param.key)
                HStack(spacing: 8) {
                    Image(systemName: met ? "checkmark" : "xmark")
                        .font(.caption)
                        .foregroundStyle(met ? .signalGreen : .secondary.opacity(0.5))
                        .frame(width: 18)

                    Text(param.label)
                        .font(.caption)
                        .foregroundStyle(met ? .primary : .secondary.opacity(0.5))
                }
            }
        }
    }
}

// MARK: - Presence Score

private struct PresenceScoreRow: View {
    let score: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .bottom) {
                Text("\(score)")
                    .font(.title)
                    .fontWeight(.bold)
                    .foregroundStyle(.blue)

                Text("/ 100")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            ProgressView(value: Double(score), total: 100)

            Text("Affects block production selection probability only. Does not affect mining rewards.")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - Earnings

private struct EarningsRow: View {
    let label: String
    let amountLux: Int64

    var body: some View {
        HStack {
            Text(label)
                .foregroundStyle(.secondary)
            Spacer()
            Text("\(formatGrat(amountLux)) GRAT")
                .fontWeight(.semibold)
        }
    }
}

// MARK: - Staking Section

private struct StakingSection: View {
    let stakeInfo: StakeInfo
    let onStake: () -> Void
    let onUnstake: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Minimum met indicator
            HStack {
                Image(systemName: stakeInfo.meetsMinimum ? "checkmark.circle.fill" : "xmark.circle.fill")
                    .foregroundStyle(stakeInfo.meetsMinimum ? .signalGreen : .alertRed)

                Text(stakeInfo.meetsMinimum ? "Minimum Met" : "Below Minimum")
                    .fontWeight(.semibold)
                    .foregroundStyle(stakeInfo.meetsMinimum ? .signalGreen : .alertRed)
            }

            StakeInfoRow(label: "Node Stake", amountLux: stakeInfo.nodeStakeLux)

            // WHY: Overflow is the amount above the per-node cap that flows to the
            // Network Security Pool. Users should see this so they understand the cap.
            StakeInfoRow(label: "Overflow", amountLux: stakeInfo.overflowAmountLux)
            StakeInfoRow(label: "Total Committed", amountLux: stakeInfo.totalCommittedLux)

            HStack(spacing: 12) {
                Button("Stake") { onStake() }
                    .buttonStyle(.borderedProminent)

                Button("Unstake") { onUnstake() }
                    .buttonStyle(.bordered)
            }
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.top, 4)
        }
    }
}

private struct StakeInfoRow: View {
    let label: String
    let amountLux: Int64

    var body: some View {
        HStack {
            Text(label)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            Spacer()
            Text("\(formatGrat(amountLux)) GRAT")
                .font(.subheadline)
                .fontWeight(.semibold)
        }
    }
}

// MARK: - Stake Amount Sheet

private struct StakeAmountSheet: View {
    @Environment(\.dismiss) private var dismiss
    let title: String
    let actionLabel: String
    let errorMessage: String?
    let onConfirm: (Int64) -> Void

    @State private var amountText = ""

    /// WHY: Parse as Int (whole GRAT) because the minimum stake and cap are
    /// whole-number values. Fractional staking can be added later if needed.
    private var parsedAmount: Int64? {
        guard let value = Int64(amountText), value > 0 else { return nil }
        return value
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Amount (GRAT)", text: $amountText)
                        .keyboardType(.numberPad)

                    if let error = errorMessage {
                        Text(error)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(actionLabel) {
                        if let amount = parsedAmount {
                            onConfirm(amount)
                        }
                    }
                    .disabled(parsedAmount == nil)
                }
            }
        }
    }
}
