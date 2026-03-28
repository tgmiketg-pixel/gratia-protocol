import SwiftUI
import CoreImage.CIFilterBuiltins

// ============================================================================
// WalletView — Display balance, transaction history, send GRAT, QR code
//
// Mirrors Android's WalletScreen.kt exactly:
// - Balance card with GRAT and Lux display
// - Send/Receive buttons
// - Transaction history list
// - Send dialog with address validation
// - Receive dialog with QR code generation
// ============================================================================

// MARK: - View Model

@MainActor
final class WalletViewModel: ObservableObject {
    @Published var walletInfo: WalletInfo?
    @Published var transactions: [TransactionInfo] = []
    @Published var isLoading = true
    @Published var showSendSheet = false
    @Published var showReceiveSheet = false
    @Published var errorMessage: String?

    private var pollingTask: Task<Void, Never>?

    init() {
        loadWalletData()
        startPolling()
    }

    deinit {
        pollingTask?.cancel()
    }

    /// WHY: Poll every 10 seconds so the wallet balance updates
    /// as mining rewards are credited (1 GRAT/minute).
    private func startPolling() {
        pollingTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 10_000_000_000) // 10 seconds
                await refreshQuiet()
            }
        }
    }

    private func refreshQuiet() async {
        guard GratiaCoreManager.shared.isInitialized else { return }
        do {
            let info = try GratiaCoreManager.shared.getWalletInfo()
            let txs = try GratiaCoreManager.shared.getTransactionHistory()
            walletInfo = info
            transactions = txs
        } catch {
            // Silent refresh — don't show errors for background polling
        }
    }

    func loadWalletData() {
        isLoading = true
        errorMessage = nil
        Task {
            do {
                guard GratiaCoreManager.shared.isInitialized else {
                    isLoading = false
                    return
                }
                let info = try GratiaCoreManager.shared.getWalletInfo()
                let txs = try GratiaCoreManager.shared.getTransactionHistory()
                walletInfo = info
                transactions = txs
                isLoading = false
            } catch {
                errorMessage = error.localizedDescription
                isLoading = false
            }
        }
    }

    func sendTransfer(to: String, amountLux: Int64) {
        Task {
            do {
                _ = try GratiaCoreManager.shared.sendTransfer(to: to, amountLux: amountLux)
                showSendSheet = false
                await refreshQuiet()
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }
}

// MARK: - Wallet View

struct WalletView: View {
    @StateObject private var viewModel = WalletViewModel()

    var body: some View {
        NavigationStack {
            Group {
                if viewModel.isLoading {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    walletContent
                }
            }
            .navigationTitle("Wallet")
            .sheet(isPresented: $viewModel.showSendSheet) {
                SendSheet { address, amountLux in
                    viewModel.sendTransfer(to: address, amountLux: amountLux)
                }
            }
            .sheet(isPresented: $viewModel.showReceiveSheet) {
                ReceiveSheet(address: viewModel.walletInfo?.address ?? "")
            }
        }
    }

    @ViewBuilder
    private var walletContent: some View {
        if let wallet = viewModel.walletInfo {
            List {
                // Balance card
                Section {
                    BalanceCard(
                        wallet: wallet,
                        onSend: { viewModel.showSendSheet = true },
                        onReceive: { viewModel.showReceiveSheet = true }
                    )
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)

                // Transaction history
                Section("Recent Transactions") {
                    if viewModel.transactions.isEmpty {
                        Text("No transactions yet")
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 48)
                    } else {
                        ForEach(viewModel.transactions) { tx in
                            TransactionRow(tx: tx)
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
        }
    }
}

// MARK: - Balance Card

private struct BalanceCard: View {
    let wallet: WalletInfo
    let onSend: () -> Void
    let onReceive: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Address row
            HStack {
                Text(truncateAddress(wallet.address))
                    .font(.subheadline)
                    .monospaced()
                    .foregroundStyle(.secondary)

                Spacer()

                Button {
                    UIPasteboard.general.string = wallet.address
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // Balance
            Text("\(formatGrat(wallet.balanceLux)) GRAT")
                .font(.largeTitle)
                .fontWeight(.bold)

            Text("\(wallet.balanceLux.formatted()) Lux")
                .font(.caption)
                .foregroundStyle(.secondary)

            // Action buttons
            HStack(spacing: 12) {
                Button(action: onSend) {
                    Label("Send", systemImage: "arrow.up.right")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)

                Button(action: onReceive) {
                    Label("Receive", systemImage: "qrcode")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
        }
        .padding(20)
        .background(Color.paleAmber.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

// MARK: - Transaction Row

private struct TransactionRow: View {
    let tx: TransactionInfo

    private var isReceived: Bool { tx.direction == "received" }

    private var directionColor: Color {
        isReceived ? .signalGreen : .alertRed
    }

    private var dateFormatted: String {
        let date = Date(timeIntervalSince1970: TimeInterval(tx.timestampMillis) / 1000.0)
        let formatter = DateFormatter()
        formatter.dateFormat = "MMM d, HH:mm"
        return formatter.string(from: date)
    }

    var body: some View {
        HStack {
            Image(systemName: isReceived ? "arrow.down.left" : "arrow.up.right")
                .foregroundStyle(directionColor)

            VStack(alignment: .leading) {
                Text(tx.counterparty.map { truncateAddress($0) } ?? "Mining reward")
                    .font(.subheadline)
                    .monospaced()
                    .lineLimit(1)

                Text(dateFormatted)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            VStack(alignment: .trailing) {
                Text("\(isReceived ? "+" : "-")\(formatGrat(tx.amountLux)) GRAT")
                    .font(.subheadline)
                    .fontWeight(.semibold)
                    .foregroundStyle(directionColor)

                Text(tx.status.capitalized)
                    .font(.caption2)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(statusColor(tx.status).opacity(0.15))
                    .clipShape(Capsule())
            }
        }
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "confirmed": return .blue
        case "pending": return .amberGold
        case "failed": return .alertRed
        default: return .gray
        }
    }
}

// MARK: - Send Sheet

private struct SendSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State private var toAddress = ""
    @State private var amountText = ""
    @State private var addressError: String?
    @State private var amountError: String?

    let onSend: (String, Int64) -> Void

    var body: some View {
        NavigationStack {
            Form {
                Section("Recipient") {
                    TextField("grat:...", text: $toAddress)
                        .monospaced()
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)

                    if let error = addressError {
                        Text(error)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }

                    Button("Paste from Clipboard") {
                        if let pasted = UIPasteboard.general.string {
                            toAddress = pasted
                            addressError = nil
                        }
                    }
                }

                Section("Amount") {
                    TextField("0.00", text: $amountText)
                        .keyboardType(.decimalPad)

                    if let error = amountError {
                        Text(error)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle("Send GRAT")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") { validateAndSend() }
                }
            }
        }
    }

    private func validateAndSend() {
        // Validate address — must be "grat:" followed by exactly 64 hex chars
        let addressPattern = /^grat:[0-9a-fA-F]{64}$/
        guard toAddress.wholeMatch(of: addressPattern) != nil else {
            addressError = "Invalid address format"
            return
        }

        // Validate amount
        guard let gratAmount = Double(amountText), gratAmount > 0 else {
            amountError = "Enter a valid amount"
            return
        }

        // Convert GRAT to Lux (1 GRAT = 1,000,000 Lux)
        let lux = Int64(gratAmount * 1_000_000)
        onSend(toAddress, lux)
    }
}

// MARK: - Receive Sheet

private struct ReceiveSheet: View {
    @Environment(\.dismiss) private var dismiss
    let address: String
    @State private var copied = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Spacer()

                // QR code
                if let qrImage = generateQRCode(from: address) {
                    Image(uiImage: qrImage)
                        .interpolation(.none)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 220, height: 220)
                        .padding(12)
                        .background(Color.white)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                }

                Divider()

                // Address display
                Text(address)
                    .font(.caption)
                    .monospaced()
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)

                // Copy button
                Button {
                    UIPasteboard.general.string = address
                    copied = true
                } label: {
                    Label(copied ? "Copied!" : "Copy Address", systemImage: "doc.on.doc")
                }
                .buttonStyle(.bordered)

                Spacer()
            }
            .navigationTitle("Receive GRAT")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    /// Generate a QR code UIImage from a string using Core Image.
    ///
    /// WHY: Wallet addresses are 69 characters (grat:<64 hex>), impractical
    /// to type manually. QR codes enable phone-to-phone transfers by scanning.
    private func generateQRCode(from string: String) -> UIImage? {
        let context = CIContext()
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(string.utf8)
        filter.correctionLevel = "M"

        guard let outputImage = filter.outputImage else { return nil }

        // WHY: Scale up from the native ~23x23 pixel QR to 512x512 for sharp display.
        let scale = 512.0 / outputImage.extent.width
        let scaledImage = outputImage.transformed(by: CGAffineTransform(scaleX: scale, y: scale))

        guard let cgImage = context.createCGImage(scaledImage, from: scaledImage.extent) else {
            return nil
        }
        return UIImage(cgImage: cgImage)
    }
}
