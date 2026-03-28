import SwiftUI

// ============================================================================
// NetworkView — Peer connections, consensus status, event log
//
// Mirrors Android's NetworkScreen.kt exactly:
// - Network control card (start/stop with peer count)
// - Consensus card (start/stop, slot/height/produced stats)
// - Error display
// - Network event log
// ============================================================================

// MARK: - View Model

@MainActor
final class NetworkViewModel: ObservableObject {
    @Published var isNetworkRunning = false
    @Published var peerCount = 0
    @Published var listenAddress: String?
    @Published var consensusState = "stopped"
    @Published var currentSlot: Int64 = 0
    @Published var currentHeight: Int64 = 0
    @Published var isCommitteeMember = false
    @Published var blocksProduced: Int64 = 0
    @Published var recentEvents: [String] = []
    @Published var isLoading = false
    @Published var errorMessage: String?

    private var pollingTask: Task<Void, Never>?

    // WHY: Maximum 100 events in the log to prevent unbounded memory growth.
    // Oldest events are dropped when the limit is reached.
    private let maxEventLogSize = 100

    init() {
        refreshStatus()
    }

    deinit {
        pollingTask?.cancel()
    }

    func refreshStatus() {
        guard GratiaCoreManager.shared.isInitialized else { return }
        Task {
            do {
                let netStatus = try GratiaCoreManager.shared.getNetworkStatus()
                isNetworkRunning = netStatus.isRunning
                peerCount = netStatus.peerCount
                listenAddress = netStatus.listenAddress

                let conStatus = try GratiaCoreManager.shared.getConsensusStatus()
                consensusState = conStatus.state
                currentSlot = conStatus.currentSlot
                currentHeight = conStatus.currentHeight
                isCommitteeMember = conStatus.isCommitteeMember
                blocksProduced = conStatus.blocksProduced
            } catch {}
        }
    }

    func startNetwork() {
        isLoading = true
        errorMessage = nil
        Task {
            do {
                let status = try GratiaCoreManager.shared.startNetwork()
                isNetworkRunning = status.isRunning
                peerCount = status.peerCount
                listenAddress = status.listenAddress
                isLoading = false
                startEventPolling()
            } catch {
                errorMessage = error.localizedDescription
                isLoading = false
            }
        }
    }

    func stopNetwork() {
        Task {
            do {
                try GratiaCoreManager.shared.stopNetwork()
                isNetworkRunning = false
                peerCount = 0
                listenAddress = nil
                stopEventPolling()
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    func startConsensus() {
        Task {
            do {
                let status = try GratiaCoreManager.shared.startConsensus()
                consensusState = status.state
                currentSlot = status.currentSlot
                currentHeight = status.currentHeight
                isCommitteeMember = status.isCommitteeMember
                blocksProduced = status.blocksProduced
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    func stopConsensus() {
        Task {
            do {
                try GratiaCoreManager.shared.stopConsensus()
                consensusState = "stopped"
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    // WHY: Poll for network events every 500ms so the UI stays responsive.
    // The Rust core buffers events and returns them on poll.
    private func startEventPolling() {
        pollingTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 500_000_000) // 500ms
                await pollEvents()
            }
        }
    }

    private func stopEventPolling() {
        pollingTask?.cancel()
        pollingTask = nil
    }

    private func pollEvents() async {
        guard GratiaCoreManager.shared.isInitialized else { return }
        do {
            let events = try GratiaCoreManager.shared.pollNetworkEvents()
            for event in events {
                let description: String
                switch event {
                case .peerConnected(let peerId):
                    description = "Peer connected: \(peerId.prefix(12))..."
                    peerCount += 1
                case .peerDisconnected(let peerId):
                    description = "Peer disconnected: \(peerId.prefix(12))..."
                    peerCount = max(0, peerCount - 1)
                case .blockReceived(let height, let producer):
                    description = "Block #\(height) from \(producer.prefix(12))..."
                    currentHeight = height
                case .transactionReceived(let hashHex):
                    description = "Tx received: \(hashHex.prefix(12))..."
                }
                recentEvents.insert(description, at: 0)
                if recentEvents.count > maxEventLogSize {
                    recentEvents.removeLast()
                }
            }

            // Refresh consensus status during polling
            let conStatus = try GratiaCoreManager.shared.getConsensusStatus()
            consensusState = conStatus.state
            currentSlot = conStatus.currentSlot
            currentHeight = conStatus.currentHeight
            isCommitteeMember = conStatus.isCommitteeMember
            blocksProduced = conStatus.blocksProduced
        } catch {}
    }
}

// MARK: - Network View

struct NetworkView: View {
    @StateObject private var viewModel = NetworkViewModel()

    var body: some View {
        NavigationStack {
            List {
                // Network control card
                Section {
                    NetworkControlCard(
                        isRunning: viewModel.isNetworkRunning,
                        peerCount: viewModel.peerCount,
                        listenAddress: viewModel.listenAddress,
                        isLoading: viewModel.isLoading,
                        onStart: { viewModel.startNetwork() },
                        onStop: { viewModel.stopNetwork() }
                    )
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)

                // Consensus card (only when network is running)
                if viewModel.isNetworkRunning {
                    Section("Consensus") {
                        ConsensusSection(
                            consensusState: viewModel.consensusState,
                            currentSlot: viewModel.currentSlot,
                            currentHeight: viewModel.currentHeight,
                            isCommitteeMember: viewModel.isCommitteeMember,
                            blocksProduced: viewModel.blocksProduced,
                            onStart: { viewModel.startConsensus() },
                            onStop: { viewModel.stopConsensus() }
                        )
                    }
                }

                // Error message
                if let error = viewModel.errorMessage {
                    Section {
                        Text(error)
                            .font(.subheadline)
                            .foregroundStyle(.red)
                    }
                }

                // Event log
                if !viewModel.recentEvents.isEmpty {
                    Section("Event Log") {
                        ForEach(Array(viewModel.recentEvents.enumerated()), id: \.offset) { _, event in
                            Text(event)
                                .font(.caption)
                                .monospaced()
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Network")
        }
    }
}

// MARK: - Network Control Card

private struct NetworkControlCard: View {
    let isRunning: Bool
    let peerCount: Int
    let listenAddress: String?
    let isLoading: Bool
    let onStart: () -> Void
    let onStop: () -> Void

    private var statusColor: Color {
        isRunning ? .signalGreen : .gray
    }

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 48))
                .foregroundStyle(statusColor)

            Text(isRunning ? "Network Active" : "Network Offline")
                .font(.title2)
                .fontWeight(.bold)
                .foregroundStyle(statusColor)

            if isRunning {
                HStack {
                    VStack {
                        Text("\(peerCount)")
                            .font(.title)
                            .fontWeight(.bold)
                            .foregroundStyle(.blue)
                        Text("Peers")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if let addr = listenAddress {
                    Text(addr)
                        .font(.caption)
                        .monospaced()
                        .foregroundStyle(.secondary)
                }
            }

            if isRunning {
                Button(role: .destructive) {
                    onStop()
                } label: {
                    Label("Stop Network", systemImage: "link.badge.plus")
                }
                .buttonStyle(.bordered)
            } else {
                Button {
                    onStart()
                } label: {
                    Label(isLoading ? "Starting..." : "Start Network", systemImage: "link")
                }
                .buttonStyle(.borderedProminent)
                .disabled(isLoading)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(20)
        .background(statusColor.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

// MARK: - Consensus Section

private struct ConsensusSection: View {
    let consensusState: String
    let currentSlot: Int64
    let currentHeight: Int64
    let isCommitteeMember: Bool
    let blocksProduced: Int64
    let onStart: () -> Void
    let onStop: () -> Void

    private var isActive: Bool { consensusState != "stopped" }

    private var stateColor: Color {
        switch consensusState {
        case "active": return .signalGreen
        case "producing": return .amberGold
        case "syncing": return .darkAmber
        default: return .gray
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("State")
                Spacer()
                Text(consensusState.capitalized)
                    .fontWeight(.semibold)
                    .foregroundStyle(stateColor)
            }

            if isActive {
                HStack(spacing: 24) {
                    StatColumn(label: "Height", value: "\(currentHeight)")
                    StatColumn(label: "Slot", value: "\(currentSlot)")
                    StatColumn(label: "Produced", value: "\(blocksProduced)")
                }
                .frame(maxWidth: .infinity)

                Divider()

                HStack {
                    Text("Committee member")
                    Spacer()
                    Text(isCommitteeMember ? "Yes" : "No")
                        .fontWeight(.semibold)
                        .foregroundStyle(isCommitteeMember ? .signalGreen : .secondary)
                }
            }

            if isActive {
                Button(role: .destructive) {
                    onStop()
                } label: {
                    Label("Stop Consensus", systemImage: "stop.fill")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            } else {
                Button {
                    onStart()
                } label: {
                    Label("Start Consensus", systemImage: "play.fill")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }
}

private struct StatColumn: View {
    let label: String
    let value: String

    var body: some View {
        VStack {
            Text(value)
                .font(.title3)
                .fontWeight(.bold)
                .foregroundStyle(.blue)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
