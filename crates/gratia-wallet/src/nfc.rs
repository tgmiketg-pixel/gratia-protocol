//! NFC tap-to-transact protocol.
//!
//! Implements the core UX feature that lets users pay by tapping phones together.
//! NFC transactions are standard `Transfer` payloads — not a separate transaction
//! type. Below a configurable threshold (default 10 GRAT), NFC transactions are
//! zero-fee to encourage everyday use as digital cash.
//!
//! ## Protocol Flow
//!
//! 1. Receiver advertises a `PaymentRequest` via NFC (address, optional amount/label).
//! 2. Sender reads the request, confirms, signs a standard `Transfer`, and sends
//!    a `PaymentConfirmation` back over NFC.
//! 3. Receiver acknowledges with a `PaymentAcknowledgment` and broadcasts the
//!    transaction to the network.
//!
//! Same-shard transactions achieve instant finality (~3-5 seconds).

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};

use gratia_core::error::GratiaError;
use gratia_core::types::{Address, Lux, Transaction, TxHash};

// ============================================================================
// Constants
// ============================================================================

/// Default maximum amount for zero-fee NFC transactions (in Lux).
/// WHY: 10 GRAT = 10,000,000 Lux. Below this threshold, tap-to-pay is free
/// to encourage everyday use as digital cash. Above this, standard fees apply.
pub const DEFAULT_NFC_ZERO_FEE_THRESHOLD: Lux = 10_000_000;

/// Maximum time (seconds) an NFC session stays valid after initial tap.
/// WHY: 30 seconds is generous for a payment interaction. Prevents stale
/// sessions from being reused or replayed after the parties walk away.
pub const NFC_SESSION_TIMEOUT_SECS: u64 = 30;

/// NFC protocol version. Included in session messages for forward compatibility.
/// WHY: When the protocol evolves, receivers and senders can detect version
/// mismatches and gracefully degrade or reject incompatible messages.
pub const NFC_PROTOCOL_VERSION: u8 = 1;

// ============================================================================
// Session Messages
// ============================================================================

/// Messages exchanged between two phones during an NFC tap-to-pay session.
///
/// The three-step handshake (Request -> Confirmation -> Acknowledgment) ensures
/// both parties agree on the transaction before it hits the network. The session
/// nonce ties all three messages together and prevents replay attacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NfcSessionMessage {
    /// Step 1: Receiver advertises their address and requested amount (optional).
    PaymentRequest {
        version: u8,
        receiver_address: Address,
        /// If set, the sender's UI shows this amount pre-filled.
        requested_amount: Option<Lux>,
        /// Human-readable label (e.g., "Coffee at Maria's Cafe").
        label: Option<String>,
        /// Session nonce for replay protection.
        session_nonce: [u8; 16],
        /// Timestamp of the request.
        timestamp: DateTime<Utc>,
    },
    /// Step 2: Sender confirms and sends the signed transaction.
    PaymentConfirmation {
        version: u8,
        /// The signed transaction (standard Transfer payload).
        transaction: Transaction,
        /// The session nonce from the PaymentRequest (proves this is a response to THAT tap).
        session_nonce: [u8; 16],
    },
    /// Step 3: Receiver acknowledges receipt.
    PaymentAcknowledgment {
        version: u8,
        /// Hash of the transaction that was received.
        tx_hash: TxHash,
        /// Whether the receiver will broadcast the transaction.
        will_broadcast: bool,
    },
}

// ============================================================================
// Session State Machine
// ============================================================================

/// Role of this device in an NFC payment session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfcRole {
    /// The party sending GRAT (paying).
    Sender,
    /// The party receiving GRAT (merchant / payee).
    Receiver,
}

/// State of an NFC payment session.
///
/// Transitions:
/// - Receiver: `AwaitingRequest` (created) -> creates PaymentRequest
/// - Sender:   `PendingConfirmation` (received request) -> signs tx -> `TransactionSent`
/// - Receiver: receives confirmation -> `Completed`
/// - Any:      timeout or error -> `Failed`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfcSessionState {
    /// Waiting for initial tap / payment request.
    AwaitingRequest,
    /// Payment request received, awaiting user confirmation.
    PendingConfirmation,
    /// Transaction signed and sent.
    TransactionSent,
    /// Transaction acknowledged by receiver.
    Completed,
    /// Session expired or failed.
    Failed,
}

/// Manages a single NFC tap-to-pay interaction between two phones.
///
/// Created by the receiver (merchant) when they're ready to accept payment,
/// or by the sender when they receive a `PaymentRequest` from a tap.
#[derive(Debug, Clone)]
pub struct NfcSession {
    /// Role in this session.
    pub role: NfcRole,
    /// The session nonce (generated by receiver, echoed by sender).
    pub session_nonce: [u8; 16],
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// Current state of the session.
    pub state: NfcSessionState,
    /// The transaction if one has been created/received.
    pub transaction: Option<Transaction>,
    /// Requested amount, if any (receiver sets this).
    requested_amount: Option<Lux>,
    /// Human-readable label for the payment.
    label: Option<String>,
}

impl NfcSession {
    /// Create a new receiver session (merchant waiting for payment).
    ///
    /// Generates a cryptographically random session nonce. The receiver
    /// then calls `create_payment_request` to produce the NFC message.
    pub fn new_receiver(requested_amount: Option<Lux>, label: Option<String>) -> Self {
        let mut nonce = [0u8; 16];
        rand::thread_rng().fill(&mut nonce);

        NfcSession {
            role: NfcRole::Receiver,
            session_nonce: nonce,
            created_at: Utc::now(),
            state: NfcSessionState::AwaitingRequest,
            transaction: None,
            requested_amount,
            label,
        }
    }

    /// Create the PaymentRequest message for NFC broadcast.
    ///
    /// Only valid for receiver sessions. The returned message is serialized
    /// and transmitted via NFC to the sender's phone.
    pub fn create_payment_request(
        &self,
        receiver_address: Address,
    ) -> Result<NfcSessionMessage, GratiaError> {
        if self.role != NfcRole::Receiver {
            return Err(GratiaError::Other(
                "only the receiver can create a payment request".into(),
            ));
        }

        if self.is_expired() {
            return Err(GratiaError::Other("NFC session has expired".into()));
        }

        Ok(NfcSessionMessage::PaymentRequest {
            version: NFC_PROTOCOL_VERSION,
            receiver_address,
            requested_amount: self.requested_amount,
            label: self.label.clone(),
            session_nonce: self.session_nonce,
            timestamp: self.created_at,
        })
    }

    /// Process an incoming PaymentRequest (sender side).
    ///
    /// Called when the sender's phone reads a PaymentRequest via NFC.
    /// Creates a new sender-side session that mirrors the receiver's nonce.
    pub fn receive_payment_request(
        request: &NfcSessionMessage,
    ) -> Result<Self, GratiaError> {
        match request {
            NfcSessionMessage::PaymentRequest {
                version,
                session_nonce,
                timestamp,
                requested_amount,
                label,
                ..
            } => {
                // WHY: Reject messages from incompatible protocol versions to
                // prevent silent failures or misinterpreted fields.
                if *version != NFC_PROTOCOL_VERSION {
                    return Err(GratiaError::Other(format!(
                        "unsupported NFC protocol version: {} (expected {})",
                        version, NFC_PROTOCOL_VERSION
                    )));
                }

                // Check if the request itself has expired based on its timestamp.
                let age = Utc::now().signed_duration_since(*timestamp);
                if age > Duration::seconds(NFC_SESSION_TIMEOUT_SECS as i64) {
                    return Err(GratiaError::Other(
                        "NFC payment request has expired".into(),
                    ));
                }

                Ok(NfcSession {
                    role: NfcRole::Sender,
                    session_nonce: *session_nonce,
                    created_at: Utc::now(),
                    state: NfcSessionState::PendingConfirmation,
                    transaction: None,
                    requested_amount: *requested_amount,
                    label: label.clone(),
                })
            }
            _ => Err(GratiaError::Other(
                "expected PaymentRequest message".into(),
            )),
        }
    }

    /// Check if the session has expired.
    pub fn is_expired(&self) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.created_at);
        elapsed > Duration::seconds(NFC_SESSION_TIMEOUT_SECS as i64)
    }

    /// Check if this transaction qualifies for zero-fee.
    ///
    /// WHY: NFC payments below the threshold are free to encourage tap-to-pay
    /// as everyday digital cash. The threshold is governance-adjustable via
    /// `RewardsConfig::zero_fee_nfc_threshold`.
    pub fn is_zero_fee(amount: Lux, threshold: Lux) -> bool {
        amount <= threshold
    }

    /// Determine the appropriate fee for an NFC transaction.
    ///
    /// Returns zero if the amount is at or below the zero-fee threshold,
    /// otherwise returns the standard fee.
    pub fn calculate_fee(amount: Lux, zero_fee_threshold: Lux, standard_fee: Lux) -> Lux {
        if Self::is_zero_fee(amount, zero_fee_threshold) {
            0
        } else {
            standard_fee
        }
    }

    /// Get the requested amount, if any.
    pub fn requested_amount(&self) -> Option<Lux> {
        self.requested_amount
    }

    /// Get the payment label, if any.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Validate a PaymentConfirmation against this receiver session.
    ///
    /// WHY: The receiver MUST verify that the confirmation's session_nonce
    /// matches the nonce from their PaymentRequest. Without this check, an
    /// attacker could replay a PaymentConfirmation from a previous session,
    /// or substitute a confirmation meant for a different receiver.
    pub fn validate_confirmation<'a>(
        &self,
        confirmation: &'a NfcSessionMessage,
    ) -> Result<&'a Transaction, GratiaError> {
        if self.role != NfcRole::Receiver {
            return Err(GratiaError::Other(
                "only the receiver can validate a confirmation".into(),
            ));
        }

        if self.is_expired() {
            return Err(GratiaError::Other("NFC session has expired".into()));
        }

        match confirmation {
            NfcSessionMessage::PaymentConfirmation {
                version,
                transaction,
                session_nonce,
            } => {
                if *version != NFC_PROTOCOL_VERSION {
                    return Err(GratiaError::Other(format!(
                        "unsupported NFC protocol version: {} (expected {})",
                        version, NFC_PROTOCOL_VERSION
                    )));
                }

                if *session_nonce != self.session_nonce {
                    return Err(GratiaError::Other(
                        "session nonce mismatch: confirmation does not match this payment request".into(),
                    ));
                }

                Ok(transaction)
            }
            _ => Err(GratiaError::Other(
                "expected PaymentConfirmation message".into(),
            )),
        }
    }
}

// ============================================================================
// Serialization Helpers
// ============================================================================

/// Serialize an NFC session message to bytes for NFC transmission.
///
/// Uses bincode for compact binary encoding — important because NFC data
/// transfer is bandwidth-constrained (typical NFC payload is ~4-8 KB).
pub fn serialize_nfc_message(msg: &NfcSessionMessage) -> Result<Vec<u8>, GratiaError> {
    bincode::serialize(msg).map_err(|e| GratiaError::SerializationError(e.to_string()))
}

/// Deserialize an NFC session message from bytes.
pub fn deserialize_nfc_message(bytes: &[u8]) -> Result<NfcSessionMessage, GratiaError> {
    bincode::deserialize(bytes).map_err(|e| GratiaError::SerializationError(e.to_string()))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::{Keystore, SoftwareKeystore};
    use crate::transactions::TransactionBuilder;
    use gratia_core::types::LUX_PER_GRAT;

    /// Helper: create a keystore with a generated keypair.
    fn setup_keystore() -> SoftwareKeystore {
        let mut ks = SoftwareKeystore::new();
        ks.generate_keypair().unwrap();
        ks
    }

    #[test]
    fn test_new_receiver_session() {
        let session = NfcSession::new_receiver(Some(5 * LUX_PER_GRAT), Some("Coffee".into()));

        assert_eq!(session.role, NfcRole::Receiver);
        assert_eq!(session.state, NfcSessionState::AwaitingRequest);
        assert_eq!(session.requested_amount(), Some(5 * LUX_PER_GRAT));
        assert_eq!(session.label(), Some("Coffee"));
        assert!(session.transaction.is_none());
        assert!(!session.is_expired());
        // Nonce should not be all zeros (extremely unlikely for 16 random bytes).
        assert_ne!(session.session_nonce, [0u8; 16]);
    }

    #[test]
    fn test_create_payment_request() {
        let session = NfcSession::new_receiver(Some(3 * LUX_PER_GRAT), Some("Lunch".into()));
        let receiver_addr = Address([42u8; 32]);

        let msg = session.create_payment_request(receiver_addr).unwrap();

        match msg {
            NfcSessionMessage::PaymentRequest {
                version,
                receiver_address,
                requested_amount,
                label,
                session_nonce,
                ..
            } => {
                assert_eq!(version, NFC_PROTOCOL_VERSION);
                assert_eq!(receiver_address, receiver_addr);
                assert_eq!(requested_amount, Some(3 * LUX_PER_GRAT));
                assert_eq!(label, Some("Lunch".into()));
                assert_eq!(session_nonce, session.session_nonce);
            }
            _ => panic!("expected PaymentRequest"),
        }
    }

    #[test]
    fn test_receive_payment_request() {
        let receiver_session =
            NfcSession::new_receiver(Some(7 * LUX_PER_GRAT), Some("Groceries".into()));
        let receiver_addr = Address([10u8; 32]);

        let request = receiver_session
            .create_payment_request(receiver_addr)
            .unwrap();

        let sender_session = NfcSession::receive_payment_request(&request).unwrap();

        assert_eq!(sender_session.role, NfcRole::Sender);
        assert_eq!(sender_session.state, NfcSessionState::PendingConfirmation);
        // Sender mirrors the receiver's nonce.
        assert_eq!(sender_session.session_nonce, receiver_session.session_nonce);
        assert_eq!(sender_session.requested_amount(), Some(7 * LUX_PER_GRAT));
        assert_eq!(sender_session.label(), Some("Groceries"));
    }

    #[test]
    fn test_session_expiry() {
        let mut session = NfcSession::new_receiver(None, None);

        // Session just created — should not be expired.
        assert!(!session.is_expired());

        // Manually backdate the session past the timeout.
        session.created_at =
            Utc::now() - Duration::seconds(NFC_SESSION_TIMEOUT_SECS as i64 + 1);

        assert!(session.is_expired());
    }

    #[test]
    fn test_zero_fee_below_threshold() {
        let amount = 5 * LUX_PER_GRAT; // 5 GRAT
        let threshold = DEFAULT_NFC_ZERO_FEE_THRESHOLD; // 10 GRAT

        assert!(NfcSession::is_zero_fee(amount, threshold));
        assert_eq!(NfcSession::calculate_fee(amount, threshold, 1000), 0);
    }

    #[test]
    fn test_standard_fee_above_threshold() {
        let amount = 15 * LUX_PER_GRAT; // 15 GRAT
        let threshold = DEFAULT_NFC_ZERO_FEE_THRESHOLD; // 10 GRAT
        let standard_fee = 1000;

        assert!(!NfcSession::is_zero_fee(amount, threshold));
        assert_eq!(
            NfcSession::calculate_fee(amount, threshold, standard_fee),
            standard_fee
        );
    }

    #[test]
    fn test_zero_fee_at_exact_threshold() {
        // WHY: Boundary test — 10 GRAT is exactly the threshold. The spec says
        // "below threshold" but we use <= so the threshold amount itself is free.
        let amount = DEFAULT_NFC_ZERO_FEE_THRESHOLD; // Exactly 10 GRAT
        let threshold = DEFAULT_NFC_ZERO_FEE_THRESHOLD;

        assert!(NfcSession::is_zero_fee(amount, threshold));
        assert_eq!(NfcSession::calculate_fee(amount, threshold, 1000), 0);
    }

    #[test]
    fn test_serialize_deserialize_payment_request() {
        let session = NfcSession::new_receiver(Some(2 * LUX_PER_GRAT), Some("Tea".into()));
        let receiver_addr = Address([99u8; 32]);

        let msg = session.create_payment_request(receiver_addr).unwrap();
        let bytes = serialize_nfc_message(&msg).unwrap();
        let deserialized = deserialize_nfc_message(&bytes).unwrap();

        // Round-trip should preserve all fields.
        match (&msg, &deserialized) {
            (
                NfcSessionMessage::PaymentRequest {
                    version: v1,
                    receiver_address: a1,
                    requested_amount: amt1,
                    label: l1,
                    session_nonce: n1,
                    timestamp: t1,
                },
                NfcSessionMessage::PaymentRequest {
                    version: v2,
                    receiver_address: a2,
                    requested_amount: amt2,
                    label: l2,
                    session_nonce: n2,
                    timestamp: t2,
                },
            ) => {
                assert_eq!(v1, v2);
                assert_eq!(a1, a2);
                assert_eq!(amt1, amt2);
                assert_eq!(l1, l2);
                assert_eq!(n1, n2);
                assert_eq!(t1, t2);
            }
            _ => panic!("deserialized message should be PaymentRequest"),
        }
    }

    #[test]
    fn test_serialize_deserialize_payment_confirmation() {
        let ks = setup_keystore();
        let receiver_addr = Address([50u8; 32]);

        // Build a real signed transaction.
        let builder = TransactionBuilder::new(&ks, 0, 0);
        let tx = builder.build_transfer(receiver_addr, 5 * LUX_PER_GRAT).unwrap();

        let nonce = [7u8; 16];
        let msg = NfcSessionMessage::PaymentConfirmation {
            version: NFC_PROTOCOL_VERSION,
            transaction: tx.clone(),
            session_nonce: nonce,
        };

        let bytes = serialize_nfc_message(&msg).unwrap();
        let deserialized = deserialize_nfc_message(&bytes).unwrap();

        match deserialized {
            NfcSessionMessage::PaymentConfirmation {
                version,
                transaction,
                session_nonce,
            } => {
                assert_eq!(version, NFC_PROTOCOL_VERSION);
                assert_eq!(session_nonce, nonce);
                assert_eq!(transaction.hash.0, tx.hash.0);
                assert_eq!(transaction.nonce, tx.nonce);
                assert_eq!(transaction.fee, tx.fee);
            }
            _ => panic!("deserialized message should be PaymentConfirmation"),
        }
    }

    #[test]
    fn test_session_nonce_matches() {
        // Receiver generates a nonce; sender must echo it in confirmation.
        let receiver_session = NfcSession::new_receiver(Some(1 * LUX_PER_GRAT), None);
        let receiver_addr = Address([77u8; 32]);

        let request = receiver_session
            .create_payment_request(receiver_addr)
            .unwrap();

        // Extract nonce from the request.
        let request_nonce = match &request {
            NfcSessionMessage::PaymentRequest { session_nonce, .. } => *session_nonce,
            _ => panic!("expected PaymentRequest"),
        };

        // Sender creates session from request — nonce should match.
        let sender_session = NfcSession::receive_payment_request(&request).unwrap();
        assert_eq!(sender_session.session_nonce, request_nonce);
        assert_eq!(sender_session.session_nonce, receiver_session.session_nonce);

        // When sender builds a PaymentConfirmation, the nonce should be echoed.
        let ks = setup_keystore();
        let builder = TransactionBuilder::new(&ks, 0, 0);
        let tx = builder
            .build_transfer(receiver_addr, 1 * LUX_PER_GRAT)
            .unwrap();

        let confirmation = NfcSessionMessage::PaymentConfirmation {
            version: NFC_PROTOCOL_VERSION,
            transaction: tx,
            session_nonce: sender_session.session_nonce,
        };

        match confirmation {
            NfcSessionMessage::PaymentConfirmation { session_nonce, .. } => {
                assert_eq!(session_nonce, receiver_session.session_nonce);
            }
            _ => panic!("expected PaymentConfirmation"),
        }
    }

    #[test]
    fn test_protocol_version() {
        assert_eq!(NFC_PROTOCOL_VERSION, 1);

        let session = NfcSession::new_receiver(None, None);
        let msg = session
            .create_payment_request(Address([1u8; 32]))
            .unwrap();

        match msg {
            NfcSessionMessage::PaymentRequest { version, .. } => {
                assert_eq!(version, NFC_PROTOCOL_VERSION);
            }
            _ => panic!("expected PaymentRequest"),
        }

        // A message with version 99 should be rejected by receive_payment_request.
        let bad_msg = NfcSessionMessage::PaymentRequest {
            version: 99,
            receiver_address: Address([1u8; 32]),
            requested_amount: None,
            label: None,
            session_nonce: [0u8; 16],
            timestamp: Utc::now(),
        };

        let result = NfcSession::receive_payment_request(&bad_msg);
        assert!(result.is_err());
    }

    #[test]
    fn test_sender_cannot_create_payment_request() {
        // Only receivers should create payment requests.
        let receiver_session = NfcSession::new_receiver(None, None);
        let request = receiver_session
            .create_payment_request(Address([1u8; 32]))
            .unwrap();

        let sender_session = NfcSession::receive_payment_request(&request).unwrap();

        let result = sender_session.create_payment_request(Address([2u8; 32]));
        assert!(result.is_err());
    }
}
