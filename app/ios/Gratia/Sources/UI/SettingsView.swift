import SwiftUI

// ============================================================================
// SettingsView — Wallet management, staking, privacy, inheritance, about
//
// Mirrors Android's SettingsScreen.kt exactly:
// - Wallet recovery options with seed phrase export
// - Staking controls (stake/unstake)
// - Privacy settings (location granularity, optional sensors)
// - Inheritance (dead-man switch with beneficiary)
// - About section
// ============================================================================

// MARK: - Location Granularity

enum LocationGranularity: String, CaseIterable {
    case precise = "Precise"
    case approximate = "Approximate (city-level)"
    case coarse = "Coarse (region-level)"
}

// MARK: - View Model

@MainActor
final class SettingsViewModel: ObservableObject {
    @Published var stakeInfo: StakeInfo?
    @Published var showExportSeedConfirmation = false
    @Published var exportedSeedPhrase: String?
    @Published var showStakeSheet = false
    @Published var showUnstakeSheet = false
    @Published var locationGranularity: LocationGranularity = .approximate
    @Published var cameraHashEnabled = false
    @Published var microphoneFingerprintEnabled = false
    @Published var inheritanceEnabled = false
    @Published var beneficiaryAddress = ""
    @Published var showBeneficiarySheet = false
    @Published var isLoading = true

    // About section
    let appVersion = "0.1.0-alpha"
    var nodeId: String {
        (try? GratiaCoreManager.shared.getWalletInfo().address) ?? "Unknown"
    }
    var participationDays: Int64 {
        (try? GratiaCoreManager.shared.getProofOfLifeStatus().consecutiveDays) ?? 0
    }

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
                stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
            } catch {}
            isLoading = false
        }
    }

    func exportSeedPhrase() {
        Task {
            do {
                exportedSeedPhrase = try GratiaCoreManager.shared.exportSeedPhrase()
                showExportSeedConfirmation = false
            } catch {}
        }
    }

    func stake(_ amountLux: Int64) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.stake(amountLux: amountLux)
                stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
                showStakeSheet = false
            } catch {}
        }
    }

    func unstake(_ amountLux: Int64) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.unstake(amountLux: amountLux)
                stakeInfo = try GratiaCoreManager.shared.getStakeInfo()
                showUnstakeSheet = false
            } catch {}
        }
    }
}

// MARK: - Settings View

struct SettingsView: View {
    @StateObject private var viewModel = SettingsViewModel()

    var body: some View {
        NavigationStack {
            Group {
                if viewModel.isLoading {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    settingsContent
                }
            }
            .navigationTitle("Settings")
            // Export seed confirmation
            .alert("Export Seed Phrase", isPresented: $viewModel.showExportSeedConfirmation) {
                Button("I Understand, Export", role: .destructive) {
                    viewModel.exportSeedPhrase()
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This is NOT the recommended recovery method. The Proof of Life behavioral recovery is more secure. Only export your seed phrase if you understand the risks of storing it.")
            }
            // Seed phrase display
            .sheet(item: Binding(
                get: { viewModel.exportedSeedPhrase.map { SeedPhraseWrapper(phrase: $0) } },
                set: { _ in viewModel.exportedSeedPhrase = nil }
            )) { wrapper in
                SeedPhraseSheet(seedPhrase: wrapper.phrase)
            }
            // Stake sheet
            .sheet(isPresented: $viewModel.showStakeSheet) {
                AmountSheet(
                    title: "Stake GRAT",
                    description: "Enter the amount to stake. Stakes above the per-node cap (1,000 GRAT) overflow to the Network Security Pool.",
                    actionLabel: "Stake"
                ) { amountLux in
                    viewModel.stake(amountLux)
                }
            }
            // Unstake sheet
            .sheet(isPresented: $viewModel.showUnstakeSheet) {
                AmountSheet(
                    title: "Unstake GRAT",
                    description: "Enter the amount to unstake. Overflow stake is removed first. Subject to cooldown period.",
                    actionLabel: "Unstake"
                ) { amountLux in
                    viewModel.unstake(amountLux)
                }
            }
            // Beneficiary sheet
            .sheet(isPresented: $viewModel.showBeneficiarySheet) {
                BeneficiarySheet(
                    currentAddress: viewModel.beneficiaryAddress
                ) { address in
                    viewModel.beneficiaryAddress = address
                    viewModel.showBeneficiarySheet = false
                }
            }
        }
    }

    private var settingsContent: some View {
        List {
            // Wallet section
            Section("Wallet") {
                WalletSettingsContent(onExportSeed: {
                    viewModel.showExportSeedConfirmation = true
                })
            }

            // Staking section
            Section("Staking") {
                StakingSettingsContent(
                    stakeInfo: viewModel.stakeInfo,
                    onStake: { viewModel.showStakeSheet = true },
                    onUnstake: { viewModel.showUnstakeSheet = true }
                )
            }

            // Privacy section
            Section("Privacy") {
                PrivacySettingsContent(
                    locationGranularity: $viewModel.locationGranularity,
                    cameraHashEnabled: $viewModel.cameraHashEnabled,
                    micFingerprintEnabled: $viewModel.microphoneFingerprintEnabled
                )
            }

            // Inheritance section
            Section("Inheritance") {
                InheritanceSettingsContent(
                    enabled: $viewModel.inheritanceEnabled,
                    beneficiaryAddress: viewModel.beneficiaryAddress,
                    onEditBeneficiary: { viewModel.showBeneficiarySheet = true }
                )
            }

            // About section
            Section("About") {
                AboutRow(label: "App version", value: viewModel.appVersion)
                AboutRow(label: "Node ID", value: truncateAddress(viewModel.nodeId))
                AboutRow(label: "Participation", value: "\(viewModel.participationDays) days")
            }
        }
        .listStyle(.insetGrouped)
    }
}

// MARK: - Wallet Settings

private struct WalletSettingsContent: View {
    let onExportSeed: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Recovery Options")
                .font(.subheadline)
                .fontWeight(.medium)

            Text("Your wallet is secured by the device's Secure Enclave and your Proof of Life behavioral profile. If you lose this device, recovery uses PoL behavioral matching over 7-14 days on a new device.")
                .font(.caption)
                .foregroundStyle(.secondary)

            Button(role: .destructive) {
                onExportSeed()
            } label: {
                Label("Export Seed Phrase", systemImage: "key")
            }

            Text("Optional. Store securely if exported. This is NOT the recommended recovery method.")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - Staking Settings

private struct StakingSettingsContent: View {
    let stakeInfo: StakeInfo?
    let onStake: () -> Void
    let onUnstake: () -> Void

    var body: some View {
        if let info = stakeInfo {
            HStack {
                Text("Effective stake")
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(formatGrat(info.nodeStakeLux)) GRAT")
                    .fontWeight(.semibold)
            }
            if info.overflowAmountLux > 0 {
                HStack {
                    Text("Overflow to pool")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text("\(formatGrat(info.overflowAmountLux)) GRAT")
                        .fontWeight(.semibold)
                }
            }
            HStack {
                Text("Total committed")
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(formatGrat(info.totalCommittedLux)) GRAT")
                    .fontWeight(.semibold)
            }
            HStack {
                Text("Minimum met")
                    .foregroundStyle(.secondary)
                Spacer()
                Text(info.meetsMinimum ? "Yes" : "No")
                    .fontWeight(.semibold)
            }
        } else {
            Text("No stake active")
                .foregroundStyle(.secondary)
        }

        HStack(spacing: 12) {
            Button("Stake") { onStake() }
                .buttonStyle(.borderedProminent)
            Button("Unstake") { onUnstake() }
                .buttonStyle(.bordered)
        }
    }
}

// MARK: - Privacy Settings

private struct PrivacySettingsContent: View {
    @Binding var locationGranularity: LocationGranularity
    @Binding var cameraHashEnabled: Bool
    @Binding var micFingerprintEnabled: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("All sensor data is processed on-device. Raw data never leaves your phone. Zero-knowledge proofs are used for all attestations.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }

        Picker("Location Granularity", selection: $locationGranularity) {
            ForEach(LocationGranularity.allCases, id: \.self) { option in
                Text(option.rawValue).tag(option)
            }
        }

        Toggle(isOn: $cameraHashEnabled) {
            VStack(alignment: .leading) {
                Text("Camera environment hash")
                Text("Contributes to Presence Score (+4). Only a hash of the environment is used, never images.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }

        Toggle(isOn: $micFingerprintEnabled) {
            VStack(alignment: .leading) {
                Text("Microphone ambient fingerprint")
                Text("Contributes to Presence Score (+4). Only an acoustic fingerprint is used, never audio content.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Inheritance Settings

private struct InheritanceSettingsContent: View {
    @Binding var enabled: Bool
    let beneficiaryAddress: String
    let onEditBeneficiary: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Designate a beneficiary wallet that receives your funds if the 365-day dead-man switch triggers. Your daily Proof of Life activity resets the timer automatically.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }

        Toggle("Enable dead-man switch", isOn: $enabled)

        if enabled {
            HStack {
                VStack(alignment: .leading) {
                    Text("Beneficiary")
                        .font(.subheadline)
                        .fontWeight(.medium)
                    Text(beneficiaryAddress.isEmpty ? "Not set" : truncateAddress(beneficiaryAddress))
                        .font(.caption)
                        .monospaced()
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button(beneficiaryAddress.isEmpty ? "Set" : "Change") {
                    onEditBeneficiary()
                }
                .buttonStyle(.bordered)
            }
        }
    }
}

// MARK: - About Row

private struct AboutRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack {
            Text(label)
                .foregroundStyle(.secondary)
            Spacer()
            Text(value)
                .monospaced()
        }
    }
}

// MARK: - Seed Phrase Sheet

/// Wrapper to conform String to Identifiable for sheet presentation.
private struct SeedPhraseWrapper: Identifiable {
    let phrase: String
    var id: String { phrase }
}

private struct SeedPhraseSheet: View {
    @Environment(\.dismiss) private var dismiss
    let seedPhrase: String
    @State private var copied = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Image(systemName: "key.fill")
                    .font(.largeTitle)
                    .foregroundStyle(.red)

                Text("Your Seed Phrase")
                    .font(.title2)
                    .fontWeight(.bold)

                Text("Store this securely. Anyone with this key can access your wallet. Never share it.")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)

                Text(seedPhrase)
                    .font(.caption)
                    .monospaced()
                    .padding(12)
                    .background(Color(.systemGray6))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .padding(.horizontal)

                Button {
                    UIPasteboard.general.string = seedPhrase
                    copied = true
                } label: {
                    Label(copied ? "Copied!" : "Copy to Clipboard", systemImage: "doc.on.doc")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .padding(.horizontal)

                Spacer()
            }
            .padding(.top, 24)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

// MARK: - Amount Sheet

private struct AmountSheet: View {
    @Environment(\.dismiss) private var dismiss
    let title: String
    let description: String
    let actionLabel: String
    let onAction: (Int64) -> Void

    @State private var amountText = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(description)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section {
                    TextField("Amount (GRAT)", text: $amountText)
                        .keyboardType(.decimalPad)

                    if let error = error {
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
                        guard let amount = Double(amountText), amount > 0 else {
                            error = "Enter a valid amount"
                            return
                        }
                        let lux = Int64(amount * 1_000_000)
                        onAction(lux)
                    }
                }
            }
        }
    }
}

// MARK: - Beneficiary Sheet

private struct BeneficiarySheet: View {
    @Environment(\.dismiss) private var dismiss
    let currentAddress: String
    let onSave: (String) -> Void

    @State private var address: String
    @State private var error: String?

    init(currentAddress: String, onSave: @escaping (String) -> Void) {
        self.currentAddress = currentAddress
        self.onSave = onSave
        _address = State(initialValue: currentAddress)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("Enter the wallet address that should receive your funds after 365 days of inactivity.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section {
                    TextField("grat:...", text: $address)
                        .monospaced()
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)

                    if let error = error {
                        Text(error)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle("Set Beneficiary")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        guard address.hasPrefix("grat:"), address.count >= 10 else {
                            error = "Invalid address format"
                            return
                        }
                        onSave(address)
                    }
                }
            }
        }
    }
}
