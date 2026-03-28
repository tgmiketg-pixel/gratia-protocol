import Foundation
import CoreNFC

// ============================================================================
// NfcManager — Core NFC wrapper for tap-to-transact
//
// Mirrors Android's NfcManager.kt:
// - NFCNDEFReaderSession for reading wallet addresses from NFC tags
// - NFCNDEFMessage writing for payment request tags
//
// WHY: NFC enables instant phone-to-phone GRAT transfers by tapping devices
// together. One phone displays a QR code OR broadcasts via NFC; the other
// phone reads the address and opens the send dialog pre-filled.
//
// iOS NFC limitations:
// - Background NFC tag reading requires NFC entitlement and Info.plist config
// - HCE (Host Card Emulation) is not available on iOS — only tag reading
// - For phone-to-phone transfers, the sending phone reads the receiver's
//   NFC tag (or QR code). The receiver cannot actively broadcast like Android HCE.
// ============================================================================

final class NfcManager: NSObject, ObservableObject, NFCNDEFReaderSessionDelegate {

    private let logger = GratiaLogger(tag: "GratiaNfcManager")

    private var readerSession: NFCNDEFReaderSession?

    /// Callback when a wallet address is read from an NFC tag.
    var onAddressRead: ((String) -> Void)?

    /// Whether NFC is available on this device.
    var isAvailable: Bool {
        NFCNDEFReaderSession.readingAvailable
    }

    /// Start an NFC reader session to scan for Gratia wallet address tags.
    ///
    /// WHY: Each reader session is single-use on iOS. A new session must be
    /// created for each scan attempt. The user sees a system NFC scanning sheet.
    func startReading() {
        guard isAvailable else {
            logger.warning("NFC not available on this device")
            return
        }

        // WHY: invalidateAfterFirstRead = true means the session automatically
        // closes after reading one tag, which is the expected UX for a payment scan.
        readerSession = NFCNDEFReaderSession(
            delegate: self,
            queue: nil,
            invalidateAfterFirstRead: true
        )
        readerSession?.alertMessage = "Hold your phone near the other device to receive their wallet address."
        readerSession?.begin()
        logger.info("NFC reader session started")
    }

    /// Write a wallet address to an NFC tag as an NDEF text record.
    ///
    /// WHY: This allows creating physical NFC tags with wallet addresses
    /// for point-of-sale or donation scenarios. Tap the tag to pre-fill
    /// the send dialog with the merchant's address.
    func writeAddress(_ address: String) {
        guard isAvailable else {
            logger.warning("NFC not available on this device")
            return
        }

        let session = NFCNDEFReaderSession(
            delegate: self,
            queue: nil,
            invalidateAfterFirstRead: false
        )
        session.alertMessage = "Hold your phone near the NFC tag to write your wallet address."

        // WHY: Store the address to write in a property so the delegate can
        // access it when a tag is detected.
        pendingWriteAddress = address
        pendingWriteSession = session
        session.begin()
        logger.info("NFC writer session started")
    }

    private var pendingWriteAddress: String?
    private var pendingWriteSession: NFCNDEFReaderSession?

    // MARK: - NFCNDEFReaderSessionDelegate

    func readerSession(_ session: NFCNDEFReaderSession, didDetectNDEFs messages: [NFCNDEFMessage]) {
        // Reading mode — extract wallet address from NDEF records.
        for message in messages {
            for record in message.records {
                guard record.typeNameFormat == .nfcWellKnown,
                      let payload = String(data: record.payload, encoding: .utf8) else {
                    continue
                }

                // WHY: NDEF text records have a 1-byte language code length prefix
                // followed by the language code and then the actual text.
                // We strip the prefix to get the raw address string.
                let text = extractNdefText(from: record)

                // WHY: Basic validation — Gratia addresses start with "grat:"
                // and are 69 chars (grat: + 64 hex). Reject garbage data.
                if let text = text, text.hasPrefix("grat:"), text.count >= 10 {
                    logger.info("NFC read wallet address: \(text.prefix(12))...")
                    DispatchQueue.main.async {
                        self.onAddressRead?(text)
                    }
                    return
                }
            }
        }
        logger.warning("NFC tag does not contain a valid Gratia address")
    }

    func readerSession(_ session: NFCNDEFReaderSession, didDetect tags: [NFCNDEFTag]) {
        // Writing mode
        guard let address = pendingWriteAddress, let tag = tags.first else {
            session.invalidate()
            return
        }

        session.connect(to: tag) { [weak self] error in
            guard let self = self else { return }

            if let error = error {
                self.logger.error("NFC connect failed: \(error.localizedDescription)")
                session.invalidate(errorMessage: "Connection failed")
                return
            }

            tag.queryNDEFStatus { status, _, error in
                guard status == .readWrite else {
                    self.logger.warning("NFC tag is not writable")
                    session.invalidate(errorMessage: "Tag is not writable")
                    return
                }

                // Create NDEF text record with the wallet address
                let payload = NFCNDEFPayload.wellKnownTypeTextPayload(
                    string: address,
                    locale: Locale(identifier: "en")
                )!

                let message = NFCNDEFMessage(records: [payload])

                tag.writeNDEF(message) { error in
                    if let error = error {
                        self.logger.error("NFC write failed: \(error.localizedDescription)")
                        session.invalidate(errorMessage: "Write failed")
                    } else {
                        self.logger.info("NFC write successful: \(address.prefix(12))...")
                        session.alertMessage = "Address written successfully!"
                        session.invalidate()
                    }
                    self.pendingWriteAddress = nil
                    self.pendingWriteSession = nil
                }
            }
        }
    }

    func readerSessionDidBecomeActive(_ session: NFCNDEFReaderSession) {
        logger.debug("NFC reader session active")
    }

    func readerSession(_ session: NFCNDEFReaderSession, didInvalidateWithError error: Error) {
        // WHY: Session invalidation is normal (e.g., user cancelled, read complete).
        // Only log actual errors, not expected lifecycle events.
        let nfcError = error as? NFCReaderError
        if nfcError?.code != .readerSessionInvalidationErrorFirstNDEFTagRead &&
           nfcError?.code != .readerSessionInvalidationErrorUserCanceled {
            logger.warning("NFC session invalidated: \(error.localizedDescription)")
        }
        pendingWriteAddress = nil
        pendingWriteSession = nil
    }

    // MARK: - Private

    /// Extract text content from an NDEF text record.
    ///
    /// WHY: NDEF text records (type "T") have a header byte encoding the
    /// language code length, then the language code, then the actual text.
    /// We need to strip this header to get the raw wallet address.
    private func extractNdefText(from record: NFCNDEFPayload) -> String? {
        let payload = record.payload
        guard payload.count > 1 else { return nil }

        let languageCodeLength = Int(payload[0] & 0x3F)
        guard payload.count > 1 + languageCodeLength else { return nil }

        let textData = payload[(1 + languageCodeLength)...]
        return String(data: textData, encoding: .utf8)
    }
}
