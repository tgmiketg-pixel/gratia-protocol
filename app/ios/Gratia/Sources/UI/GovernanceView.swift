import SwiftUI

// ============================================================================
// GovernanceView — Proposals, voting, polls
//
// Mirrors Android's GovernanceScreen.kt exactly:
// - Tab bar: Proposals | Polls
// - Proposal list with status chips and vote bars
// - Proposal detail with vote buttons (For/Against/Abstain)
// - Poll list with options and voter counts
// - Poll detail with vote buttons per option
// - Create dialog for both proposals and polls
// ============================================================================

// MARK: - Data Models

struct Proposal: Identifiable {
    let id: String
    let title: String
    let description: String
    let status: String
    let votesFor: Int
    let votesAgainst: Int
    let votesAbstain: Int
    let discussionEndMillis: Int64
    let votingEndMillis: Int64
    let submittedByAddress: String
}

struct Poll: Identifiable {
    let id: String
    let question: String
    let options: [String]
    let votes: [Int]
    let endMillis: Int64
    let createdByAddress: String
    let totalVoters: Int
}

// MARK: - View Model

@MainActor
final class GovernanceViewModel: ObservableObject {
    @Published var proposals: [Proposal] = []
    @Published var polls: [Poll] = []
    @Published var selectedProposal: Proposal?
    @Published var selectedPoll: Poll?
    @Published var isLoading = true
    @Published var showCreateSheet = false

    init() {
        loadData()
    }

    func loadData() {
        isLoading = true
        Task {
            guard GratiaCoreManager.shared.isInitialized else {
                isLoading = false
                return
            }
            do {
                let bridgeProposals = try GratiaCoreManager.shared.getProposals()
                proposals = bridgeProposals.map { bp in
                    Proposal(
                        id: bp.idHex, title: bp.title, description: bp.description,
                        status: bp.status,
                        votesFor: Int(bp.votesYes), votesAgainst: Int(bp.votesNo),
                        votesAbstain: Int(bp.votesAbstain),
                        discussionEndMillis: bp.discussionEndMillis,
                        votingEndMillis: bp.votingEndMillis,
                        submittedByAddress: bp.submittedBy
                    )
                }

                let bridgePolls = try GratiaCoreManager.shared.getPolls()
                polls = bridgePolls.map { bp in
                    Poll(
                        id: bp.idHex, question: bp.question, options: bp.options,
                        votes: bp.votes.map { Int($0) },
                        endMillis: bp.endMillis, createdByAddress: bp.createdBy,
                        totalVoters: Int(bp.totalVoters)
                    )
                }
            } catch {}
            isLoading = false
        }
    }

    func voteOnProposal(_ proposalId: String, vote: String) {
        Task {
            do {
                try GratiaCoreManager.shared.voteOnProposal(proposalIdHex: proposalId, vote: vote)
                loadData()
            } catch {}
        }
    }

    func voteOnPoll(_ pollId: String, optionIndex: Int) {
        Task {
            do {
                try GratiaCoreManager.shared.voteOnPoll(pollIdHex: pollId, optionIndex: optionIndex)
                loadData()
            } catch {}
        }
    }

    func createProposal(title: String, description: String) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.submitProposal(title: title, description: description)
                loadData()
            } catch {}
        }
    }

    func createPoll(question: String, options: [String]) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.createPoll(question: question, options: options)
                loadData()
            } catch {}
        }
    }
}

// MARK: - Governance View

struct GovernanceView: View {
    @StateObject private var viewModel = GovernanceViewModel()
    @State private var selectedTab = 0

    var body: some View {
        NavigationStack {
            Group {
                if let proposal = viewModel.selectedProposal {
                    ProposalDetailView(
                        proposal: proposal,
                        onBack: { viewModel.selectedProposal = nil },
                        onVote: { vote in viewModel.voteOnProposal(proposal.id, vote: vote) }
                    )
                } else if let poll = viewModel.selectedPoll {
                    PollDetailView(
                        poll: poll,
                        onBack: { viewModel.selectedPoll = nil },
                        onVote: { idx in viewModel.voteOnPoll(poll.id, optionIndex: idx) }
                    )
                } else {
                    governanceList
                }
            }
            .navigationTitle("Governance")
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    if viewModel.selectedProposal == nil && viewModel.selectedPoll == nil {
                        Button {
                            viewModel.showCreateSheet = true
                        } label: {
                            Image(systemName: "plus")
                        }
                    }
                }
            }
            .sheet(isPresented: $viewModel.showCreateSheet) {
                CreateGovernanceSheet(
                    selectedTab: selectedTab,
                    onCreateProposal: { title, desc in
                        viewModel.createProposal(title: title, description: desc)
                        viewModel.showCreateSheet = false
                    },
                    onCreatePoll: { question, options in
                        viewModel.createPoll(question: question, options: options)
                        viewModel.showCreateSheet = false
                    }
                )
            }
        }
    }

    @ViewBuilder
    private var governanceList: some View {
        VStack(spacing: 0) {
            Picker("", selection: $selectedTab) {
                Text("Proposals").tag(0)
                Text("Polls").tag(1)
            }
            .pickerStyle(.segmented)
            .padding()

            if viewModel.isLoading {
                Spacer()
                ProgressView()
                Spacer()
            } else {
                if selectedTab == 0 {
                    proposalsList
                } else {
                    pollsList
                }
            }
        }
    }

    @ViewBuilder
    private var proposalsList: some View {
        if viewModel.proposals.isEmpty {
            emptyState("No proposals yet")
        } else {
            List(viewModel.proposals) { proposal in
                ProposalRow(proposal: proposal)
                    .onTapGesture { viewModel.selectedProposal = proposal }
            }
            .listStyle(.insetGrouped)
        }
    }

    @ViewBuilder
    private var pollsList: some View {
        if viewModel.polls.isEmpty {
            emptyState("No polls yet")
        } else {
            List(viewModel.polls) { poll in
                PollRow(poll: poll)
                    .onTapGesture { viewModel.selectedPoll = poll }
            }
            .listStyle(.insetGrouped)
        }
    }

    private func emptyState(_ message: String) -> some View {
        Text(message)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

// MARK: - Proposal Row

private struct ProposalRow: View {
    let proposal: Proposal

    private var totalVotes: Int {
        proposal.votesFor + proposal.votesAgainst + proposal.votesAbstain
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(proposal.title)
                    .font(.subheadline)
                    .fontWeight(.semibold)
                    .lineLimit(2)

                Spacer()

                StatusChip(status: proposal.status)
            }

            if totalVotes > 0 {
                VoteBar(votesFor: proposal.votesFor, votesAgainst: proposal.votesAgainst, total: totalVotes)
                Text("\(totalVotes) total votes")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Poll Row

private struct PollRow: View {
    let poll: Poll

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(poll.question)
                .font(.subheadline)
                .fontWeight(.semibold)

            HStack {
                Text("\(poll.options.count) options")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(poll.totalVoters) voters")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(timeRemaining(poll.endMillis))
                    .font(.caption)
                    .fontWeight(.medium)
                    .foregroundStyle(.blue)
            }
        }
    }
}

// MARK: - Proposal Detail View

private struct ProposalDetailView: View {
    let proposal: Proposal
    let onBack: () -> Void
    let onVote: (String) -> Void

    private var totalVotes: Int {
        proposal.votesFor + proposal.votesAgainst + proposal.votesAbstain
    }

    private let dateFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "MMM d, yyyy"
        return f
    }()

    var body: some View {
        List {
            // Title and status
            Section {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .top) {
                        Text(proposal.title)
                            .font(.title2)
                            .fontWeight(.bold)
                        Spacer()
                        StatusChip(status: proposal.status)
                    }
                    Text("by \(truncateAddress(proposal.submittedByAddress))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // Description
            Section("Description") {
                Text(proposal.description)
            }

            // Timeline
            Section("Timeline") {
                HStack {
                    Text("Discussion ends")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text(dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(proposal.discussionEndMillis) / 1000)))
                }
                HStack {
                    Text("Voting ends")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text(dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(proposal.votingEndMillis) / 1000)))
                }
            }

            // Results
            if totalVotes > 0 {
                Section("Results") {
                    VoteResultRow(label: "For", count: proposal.votesFor, total: totalVotes, color: .signalGreen)
                    VoteResultRow(label: "Against", count: proposal.votesAgainst, total: totalVotes, color: .alertRed)
                    VoteResultRow(label: "Abstain", count: proposal.votesAbstain, total: totalVotes, color: .agedGold)
                    Text("\(totalVotes) total votes")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // Vote buttons (only during voting phase)
            if proposal.status == "voting" {
                Section {
                    VStack(spacing: 8) {
                        Text("Cast Your Vote")
                            .font(.subheadline)
                            .fontWeight(.semibold)
                        Text("One phone, one vote")
                            .font(.caption)
                            .foregroundStyle(.secondary)

                        HStack(spacing: 8) {
                            Button("For") { onVote("for") }
                                .buttonStyle(.bordered)
                            Button("Against") { onVote("against") }
                                .buttonStyle(.bordered)
                            Button("Abstain") { onVote("abstain") }
                                .buttonStyle(.bordered)
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
            }
        }
        .listStyle(.insetGrouped)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button { onBack() } label: {
                    Image(systemName: "chevron.left")
                }
            }
        }
    }
}

// MARK: - Poll Detail View

private struct PollDetailView: View {
    let poll: Poll
    let onBack: () -> Void
    let onVote: (Int) -> Void

    var body: some View {
        List {
            // Question
            Section {
                VStack(alignment: .leading, spacing: 4) {
                    Text(poll.question)
                        .font(.title2)
                        .fontWeight(.bold)

                    HStack {
                        Text("\(poll.totalVoters) voters")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Text(timeRemaining(poll.endMillis))
                            .font(.caption)
                            .fontWeight(.medium)
                            .foregroundStyle(.blue)
                    }
                }
            }

            // Results
            Section("Results") {
                let maxVotes = poll.votes.max() ?? 1
                ForEach(Array(poll.options.enumerated()), id: \.offset) { index, option in
                    let votes = index < poll.votes.count ? poll.votes[index] : 0
                    let fraction = poll.totalVoters > 0 ? Float(votes) / Float(poll.totalVoters) : 0
                    let isLeading = votes == maxVotes && maxVotes > 0

                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(option)
                                .fontWeight(isLeading ? .semibold : .regular)
                            Spacer()
                            Text("\(votes) (\(Int(fraction * 100))%)")
                                .font(.caption)
                                .fontWeight(.medium)
                                .foregroundStyle(isLeading ? .blue : .secondary)
                        }
                        ProgressView(value: fraction, total: 1.0)
                            .tint(isLeading ? .blue : .secondary)
                    }
                }
            }

            // Vote buttons
            Section {
                VStack(spacing: 8) {
                    Text("Cast Your Vote")
                        .font(.subheadline)
                        .fontWeight(.semibold)
                    Text("One phone, one vote per poll")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    ForEach(Array(poll.options.enumerated()), id: \.offset) { index, option in
                        Button {
                            onVote(index)
                        } label: {
                            Text(option)
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                    }
                }
                .frame(maxWidth: .infinity)
            }
        }
        .listStyle(.insetGrouped)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button { onBack() } label: {
                    Image(systemName: "chevron.left")
                }
            }
        }
    }
}

// MARK: - Create Governance Sheet

private struct CreateGovernanceSheet: View {
    @Environment(\.dismiss) private var dismiss
    let selectedTab: Int
    let onCreateProposal: (String, String) -> Void
    let onCreatePoll: (String, [String]) -> Void

    @State private var title = ""
    @State private var description = ""
    @State private var question = ""
    @State private var optionsText = ""

    var body: some View {
        NavigationStack {
            Form {
                if selectedTab == 0 {
                    // Create Proposal
                    Section {
                        Text("Requires 90+ days Proof of Life history")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Section("Title") {
                        TextField("Proposal title", text: $title)
                    }
                    Section("Description") {
                        TextEditor(text: $description)
                            .frame(minHeight: 120)
                    }
                } else {
                    // Create Poll
                    Section {
                        Text("One phone, one vote per poll")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Section("Question") {
                        TextField("What should we decide?", text: $question)
                    }
                    Section("Options (one per line)") {
                        TextEditor(text: $optionsText)
                            .frame(minHeight: 120)
                    }
                }
            }
            .navigationTitle(selectedTab == 0 ? "Create Proposal" : "Create Poll")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(selectedTab == 0 ? "Submit" : "Create") {
                        if selectedTab == 0 {
                            guard !title.isEmpty, !description.isEmpty else { return }
                            onCreateProposal(title.trimmingCharacters(in: .whitespaces),
                                           description.trimmingCharacters(in: .whitespaces))
                        } else {
                            let options = optionsText.split(separator: "\n")
                                .map { $0.trimmingCharacters(in: .whitespaces) }
                                .filter { !$0.isEmpty }
                            guard !question.isEmpty, options.count >= 2 else { return }
                            onCreatePoll(question.trimmingCharacters(in: .whitespaces), options)
                        }
                    }
                }
            }
        }
    }
}

// MARK: - Shared Components

private struct StatusChip: View {
    let status: String

    private var color: Color {
        switch status {
        case "discussion": return .charcoalNavy
        case "voting": return .amberGold
        case "passed": return .signalGreen
        case "rejected": return .alertRed
        case "implemented": return .darkGoldenrod
        default: return .agedGold
        }
    }

    var body: some View {
        Text(status.capitalized)
            .font(.caption2)
            .fontWeight(.medium)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(color.opacity(0.15))
            .foregroundStyle(color)
            .clipShape(Capsule())
    }
}

private struct VoteBar: View {
    let votesFor: Int
    let votesAgainst: Int
    let total: Int

    private var forFraction: Float {
        total > 0 ? Float(votesFor) / Float(total) : 0
    }

    var body: some View {
        HStack {
            ProgressView(value: forFraction, total: 1.0)
                .tint(.signalGreen)
            Text("\(Int(forFraction * 100))%")
                .font(.caption)
                .fontWeight(.semibold)
        }
    }
}

private struct VoteResultRow: View {
    let label: String
    let count: Int
    let total: Int
    let color: Color

    private var fraction: Float {
        total > 0 ? Float(count) / Float(total) : 0
    }

    var body: some View {
        VStack(spacing: 4) {
            HStack {
                Text(label)
                Spacer()
                Text("\(count) (\(Int(fraction * 100))%)")
                    .fontWeight(.semibold)
                    .foregroundStyle(color)
            }
            ProgressView(value: fraction, total: 1.0)
                .tint(color)
        }
    }
}

/// Format a future timestamp as a human-readable "time remaining" string.
private func timeRemaining(_ endMillis: Int64) -> String {
    let diff = endMillis - Int64(Date().timeIntervalSince1970 * 1000)
    if diff <= 0 { return "Ended" }

    let hours = diff / (1000 * 60 * 60)
    let days = hours / 24
    let remainingHours = hours % 24

    if days > 0 { return "\(days)d \(remainingHours)h left" }
    if hours > 0 { return "\(hours)h left" }
    return "< 1h left"
}
