//! Wallet recovery mechanisms.
//!
//! Supports two recovery paths:
//!
//! 1. **Proof of Life behavioral matching** (primary) — A new device collects
//!    behavioral data for 7-14 days and submits a recovery claim. The protocol
//!    compares the behavioral signature against the original owner's historical
//!    profile. The original device owner can reject the claim instantly.
//!
//! 2. **Optional seed phrase** (secondary) — Opt-in only. Buried in settings,
//!    not shown during onboarding. Generates a set of random words that can
//!    reconstruct the private key on a new device.
//!
//! Additionally, an optional **inheritance** feature allows designating a
//! beneficiary wallet with a 365-day dead-man switch.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use gratia_core::error::GratiaError;
use gratia_core::types::Address;

// ============================================================================
// Recovery Claim State Machine
// ============================================================================

/// States of a wallet recovery claim.
///
/// Flow: NewClaim -> Pending -> Verified | Rejected
///
/// The original device owner can reject the claim at any point during the
/// Pending phase, which immediately cancels the recovery and unfreezes
/// the wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryState {
    /// Claim has been submitted. Behavioral data collection is beginning on
    /// the new device. The original wallet is frozen during this phase.
    NewClaim {
        claimant_address: Address,
        claimed_at: DateTime<Utc>,
    },

    /// Behavioral data is being collected and compared. This phase lasts
    /// 7-14 days depending on match confidence progression.
    Pending {
        claimant_address: Address,
        claimed_at: DateTime<Utc>,
        /// Current behavioral match confidence (0-100%).
        match_confidence: u32,
        /// Number of days of behavioral data collected so far.
        days_collected: u32,
    },

    /// Recovery verified — behavioral profile matches. Wallet ownership
    /// transfers to the new device.
    Verified {
        claimant_address: Address,
        verified_at: DateTime<Utc>,
    },

    /// Recovery rejected — either the original owner rejected the claim
    /// from their device, or the behavioral match confidence was too low
    /// after the maximum collection period.
    Rejected {
        claimant_address: Address,
        rejected_at: DateTime<Utc>,
        reason: RejectionReason,
    },
}

/// Why a recovery claim was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    /// The original device owner explicitly rejected the claim.
    OwnerRejected,
    /// Behavioral match confidence was below the required threshold
    /// after the maximum collection period (14 days).
    InsufficientBehavioralMatch { confidence: u32, required: u32 },
    /// The claim expired (claimant stopped providing behavioral data).
    Expired,
}

/// A recovery claim tracking struct held on-chain or in local state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryClaim {
    /// The wallet address being recovered.
    pub wallet_address: Address,
    /// Current state of the recovery process.
    pub state: RecoveryState,
}

/// Minimum behavioral match confidence to approve recovery (percentage).
/// WHY: 80% threshold balances security (preventing theft) against usability
/// (allowing legitimate owners whose behavior naturally drifts slightly).
const RECOVERY_MATCH_THRESHOLD: u32 = 80;

/// Minimum days of behavioral data before recovery can be approved.
/// WHY: 7 days provides enough behavioral signal to distinguish the real
/// owner from an impersonator while keeping recovery reasonably fast.
const MIN_RECOVERY_DAYS: u32 = 7;

/// Maximum days of behavioral data collection before auto-rejection.
/// WHY: 14 days is the upper bound. If confidence hasn't reached the
/// threshold by then, the behavioral signature doesn't match.
const MAX_RECOVERY_DAYS: u32 = 14;

impl RecoveryClaim {
    /// Create a new recovery claim for a wallet.
    pub fn new(wallet_address: Address, claimant_address: Address) -> Self {
        let now = Utc::now();
        RecoveryClaim {
            wallet_address,
            state: RecoveryState::NewClaim {
                claimant_address,
                claimed_at: now,
            },
        }
    }

    /// Advance the claim to the Pending state or update match confidence.
    ///
    /// Called daily as new behavioral data arrives from the claimant device.
    ///
    /// # Validation
    /// - `new_confidence` is clamped to [0, 100]
    /// - `days_collected` must be monotonically increasing (cannot go backwards)
    pub fn update_behavioral_match(
        &mut self,
        new_confidence: u32,
        days_collected: u32,
    ) -> Result<(), GratiaError> {
        // WHY: Confidence is a percentage (0-100). Values above 100 are invalid
        // and could cause misleading match results or bypass threshold checks.
        let new_confidence = new_confidence.min(100);

        match &self.state {
            RecoveryState::NewClaim {
                claimant_address,
                claimed_at,
            } => {
                self.state = RecoveryState::Pending {
                    claimant_address: *claimant_address,
                    claimed_at: *claimed_at,
                    match_confidence: new_confidence,
                    days_collected,
                };
                Ok(())
            }
            RecoveryState::Pending {
                claimant_address,
                claimed_at,
                days_collected: prev_days,
                ..
            } => {
                // WHY: days_collected must be monotonically increasing. Going backwards
                // would indicate data corruption or an attempt to reset the collection
                // window to avoid auto-rejection at MAX_RECOVERY_DAYS.
                if days_collected < *prev_days {
                    return Err(GratiaError::Other(
                        format!(
                            "days_collected must be monotonically increasing: {} < {}",
                            days_collected, prev_days
                        ),
                    ));
                }
                // Check if we should auto-verify or auto-reject
                if new_confidence >= RECOVERY_MATCH_THRESHOLD
                    && days_collected >= MIN_RECOVERY_DAYS
                {
                    self.state = RecoveryState::Verified {
                        claimant_address: *claimant_address,
                        verified_at: Utc::now(),
                    };
                } else if days_collected >= MAX_RECOVERY_DAYS {
                    // Max collection period reached without sufficient confidence
                    self.state = RecoveryState::Rejected {
                        claimant_address: *claimant_address,
                        rejected_at: Utc::now(),
                        reason: RejectionReason::InsufficientBehavioralMatch {
                            confidence: new_confidence,
                            required: RECOVERY_MATCH_THRESHOLD,
                        },
                    };
                } else {
                    self.state = RecoveryState::Pending {
                        claimant_address: *claimant_address,
                        claimed_at: *claimed_at,
                        match_confidence: new_confidence,
                        days_collected,
                    };
                }
                Ok(())
            }
            RecoveryState::Verified { .. } | RecoveryState::Rejected { .. } => {
                Err(GratiaError::Other(
                    "cannot update a finalized recovery claim".into(),
                ))
            }
        }
    }

    /// Owner rejects the recovery claim from the original device.
    /// This is instant — no waiting period.
    pub fn owner_reject(&mut self) -> Result<(), GratiaError> {
        let claimant = match &self.state {
            RecoveryState::NewClaim {
                claimant_address, ..
            } => *claimant_address,
            RecoveryState::Pending {
                claimant_address, ..
            } => *claimant_address,
            RecoveryState::Verified { .. } => {
                return Err(GratiaError::Other(
                    "cannot reject an already-verified claim".into(),
                ));
            }
            RecoveryState::Rejected { .. } => {
                return Err(GratiaError::Other(
                    "claim is already rejected".into(),
                ));
            }
        };

        self.state = RecoveryState::Rejected {
            claimant_address: claimant,
            rejected_at: Utc::now(),
            reason: RejectionReason::OwnerRejected,
        };
        Ok(())
    }

    /// Check whether this claim is in a terminal state (verified or rejected).
    pub fn is_finalized(&self) -> bool {
        matches!(
            self.state,
            RecoveryState::Verified { .. } | RecoveryState::Rejected { .. }
        )
    }

    /// Check whether the wallet should be frozen (any active claim exists).
    pub fn wallet_is_frozen(&self) -> bool {
        matches!(
            self.state,
            RecoveryState::NewClaim { .. } | RecoveryState::Pending { .. }
        )
    }
}

// ============================================================================
// Seed Phrase (Optional, Opt-In)
// ============================================================================

/// A seed phrase backup. Optional, buried in settings, not shown during onboarding.
///
/// # Implementation Note
/// This is a conceptual structure. A full BIP39-style word list is not included
/// in Phase 1. The 32-byte entropy maps to a deterministic Ed25519 keypair.
/// In production, the entropy would be encoded as a mnemonic word sequence
/// for human-friendly backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedPhrase {
    /// Raw entropy bytes (32 bytes = 256 bits of entropy).
    /// In production, these map to a 24-word mnemonic via a word list.
    entropy: Vec<u8>,
}

impl SeedPhrase {
    /// Generate a new seed phrase from a secret key's raw bytes.
    ///
    /// The entropy IS the secret key — recovering from the seed phrase
    /// directly reconstructs the Ed25519 signing key.
    pub fn from_secret_key(secret_bytes: &[u8]) -> Result<Self, GratiaError> {
        if secret_bytes.len() != 32 {
            return Err(GratiaError::Other(
                "secret key must be exactly 32 bytes".into(),
            ));
        }
        Ok(SeedPhrase {
            entropy: secret_bytes.to_vec(),
        })
    }

    /// Recover the secret key bytes from this seed phrase.
    pub fn to_secret_key_bytes(&self) -> &[u8] {
        &self.entropy
    }

    /// Return the entropy as a hex string (placeholder for mnemonic encoding).
    ///
    /// In production, this would return a space-separated word list.
    /// For Phase 1, hex is sufficient for testing the recovery flow.
    pub fn to_hex(&self) -> String {
        hex::encode(&self.entropy)
    }

    /// Parse a hex-encoded seed phrase (placeholder for mnemonic decoding).
    pub fn from_hex(hex_str: &str) -> Result<Self, GratiaError> {
        let bytes = hex::decode(hex_str)
            .map_err(|e| GratiaError::Other(format!("invalid hex seed phrase: {}", e)))?;
        Self::from_secret_key(&bytes)
    }
}

impl Drop for SeedPhrase {
    fn drop(&mut self) {
        self.entropy.zeroize();
    }
}

// ============================================================================
// Inheritance (Optional, Opt-In)
// ============================================================================

/// Optional inheritance designation with a dead-man switch.
///
/// The owner designates a beneficiary wallet. If the owner fails to
/// produce a valid Proof of Life for 365 consecutive days, the wallet
/// contents transfer to the beneficiary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InheritanceConfig {
    /// The beneficiary wallet address.
    pub beneficiary: Address,
    /// When this inheritance designation was created.
    pub designated_at: DateTime<Utc>,
    /// Dead-man switch duration. Defaults to 365 days.
    /// WHY: 365 days prevents false triggers from extended travel, illness,
    /// or simply forgetting to mine for a few months. One full year without
    /// any phone activity is a strong signal of incapacitation or death.
    pub timeout_days: u32,
    /// The last date the owner produced a valid Proof of Life.
    pub last_proof_of_life: DateTime<Utc>,
}

/// Default dead-man switch duration in days.
/// WHY: 365 days — see InheritanceConfig.timeout_days comment.
const DEFAULT_INHERITANCE_TIMEOUT_DAYS: u32 = 365;

impl InheritanceConfig {
    /// Create a new inheritance designation.
    pub fn new(beneficiary: Address) -> Self {
        let now = Utc::now();
        InheritanceConfig {
            beneficiary,
            designated_at: now,
            timeout_days: DEFAULT_INHERITANCE_TIMEOUT_DAYS,
            last_proof_of_life: now,
        }
    }

    /// Create with a custom timeout (must be at least 30 days).
    pub fn with_timeout(beneficiary: Address, timeout_days: u32) -> Result<Self, GratiaError> {
        // WHY: 30-day minimum prevents accidental or malicious short timers
        // that could trigger inheritance transfer during a brief vacation.
        if timeout_days < 30 {
            return Err(GratiaError::Other(
                "inheritance timeout must be at least 30 days".into(),
            ));
        }
        let mut config = Self::new(beneficiary);
        config.timeout_days = timeout_days;
        Ok(config)
    }

    /// Update the last Proof of Life timestamp (resets the dead-man switch).
    pub fn record_proof_of_life(&mut self) {
        self.last_proof_of_life = Utc::now();
    }

    /// Check whether the dead-man switch has triggered.
    pub fn is_triggered(&self) -> bool {
        let deadline = self.last_proof_of_life
            + Duration::days(self.timeout_days as i64);
        Utc::now() > deadline
    }

    /// Days remaining before the dead-man switch triggers.
    /// Returns 0 if already triggered.
    pub fn days_remaining(&self) -> u32 {
        let deadline = self.last_proof_of_life
            + Duration::days(self.timeout_days as i64);
        let remaining = deadline - Utc::now();
        if remaining.num_days() < 0 {
            0
        } else {
            remaining.num_days() as u32
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address(byte: u8) -> Address {
        Address([byte; 32])
    }

    // --- Recovery Claim Tests ---

    #[test]
    fn test_recovery_claim_lifecycle_verified() {
        let wallet = test_address(1);
        let claimant = test_address(2);

        let mut claim = RecoveryClaim::new(wallet, claimant);
        assert!(claim.wallet_is_frozen());
        assert!(!claim.is_finalized());

        // Day 1-6: collecting data, confidence growing
        claim.update_behavioral_match(40, 1).unwrap();
        assert!(claim.wallet_is_frozen());

        claim.update_behavioral_match(60, 4).unwrap();
        assert!(claim.wallet_is_frozen());

        // Day 7: confidence reaches threshold
        claim.update_behavioral_match(85, 7).unwrap();
        assert!(claim.is_finalized());
        assert!(!claim.wallet_is_frozen());

        match &claim.state {
            RecoveryState::Verified { .. } => {}
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn test_recovery_claim_auto_reject_after_max_days() {
        let wallet = test_address(1);
        let claimant = test_address(2);

        let mut claim = RecoveryClaim::new(wallet, claimant);

        // First update moves from NewClaim to Pending
        claim.update_behavioral_match(50, 1).unwrap();
        // 14 days pass but confidence never reaches 80%
        claim.update_behavioral_match(50, 14).unwrap();
        assert!(claim.is_finalized());

        match &claim.state {
            RecoveryState::Rejected { reason, .. } => {
                assert_eq!(
                    *reason,
                    RejectionReason::InsufficientBehavioralMatch {
                        confidence: 50,
                        required: 80,
                    }
                );
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[test]
    fn test_recovery_claim_owner_rejection() {
        let wallet = test_address(1);
        let claimant = test_address(2);

        let mut claim = RecoveryClaim::new(wallet, claimant);
        claim.update_behavioral_match(30, 2).unwrap();

        // Owner rejects from original device
        claim.owner_reject().unwrap();
        assert!(claim.is_finalized());
        assert!(!claim.wallet_is_frozen());

        match &claim.state {
            RecoveryState::Rejected { reason, .. } => {
                assert_eq!(*reason, RejectionReason::OwnerRejected);
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[test]
    fn test_cannot_update_finalized_claim() {
        let wallet = test_address(1);
        let claimant = test_address(2);

        let mut claim = RecoveryClaim::new(wallet, claimant);
        claim.owner_reject().unwrap();

        let result = claim.update_behavioral_match(90, 7);
        assert!(result.is_err());
    }

    #[test]
    fn test_high_confidence_before_min_days_stays_pending() {
        let wallet = test_address(1);
        let claimant = test_address(2);

        let mut claim = RecoveryClaim::new(wallet, claimant);

        // High confidence but only 3 days — not enough
        claim.update_behavioral_match(95, 3).unwrap();
        assert!(!claim.is_finalized());
        assert!(claim.wallet_is_frozen());
    }

    // --- Seed Phrase Tests ---

    #[test]
    fn test_seed_phrase_roundtrip() {
        let secret = [42u8; 32];
        let phrase = SeedPhrase::from_secret_key(&secret).unwrap();

        let hex_str = phrase.to_hex();
        let recovered = SeedPhrase::from_hex(&hex_str).unwrap();

        assert_eq!(phrase.to_secret_key_bytes(), recovered.to_secret_key_bytes());
        assert_eq!(recovered.to_secret_key_bytes(), &secret);
    }

    #[test]
    fn test_seed_phrase_invalid_length() {
        let result = SeedPhrase::from_secret_key(&[0u8; 16]);
        assert!(result.is_err());
    }

    #[test]
    fn test_seed_phrase_invalid_hex() {
        let result = SeedPhrase::from_hex("not_valid_hex!");
        assert!(result.is_err());
    }

    // --- Inheritance Tests ---

    #[test]
    fn test_inheritance_not_triggered_initially() {
        let config = InheritanceConfig::new(test_address(99));
        assert!(!config.is_triggered());
        assert!(config.days_remaining() > 360);
    }

    #[test]
    fn test_inheritance_custom_timeout_min_enforced() {
        let result = InheritanceConfig::with_timeout(test_address(99), 10);
        assert!(result.is_err()); // Below 30-day minimum
    }

    #[test]
    fn test_inheritance_custom_timeout_valid() {
        let config = InheritanceConfig::with_timeout(test_address(99), 180).unwrap();
        assert_eq!(config.timeout_days, 180);
        assert!(!config.is_triggered());
    }

    #[test]
    fn test_inheritance_triggered_after_timeout() {
        let mut config = InheritanceConfig::new(test_address(99));
        // Simulate last PoL being 400 days ago
        config.last_proof_of_life = Utc::now() - Duration::days(400);
        assert!(config.is_triggered());
        assert_eq!(config.days_remaining(), 0);
    }

    #[test]
    fn test_inheritance_reset_on_proof_of_life() {
        let mut config = InheritanceConfig::new(test_address(99));
        // Simulate last PoL being 300 days ago
        config.last_proof_of_life = Utc::now() - Duration::days(300);
        assert!(!config.is_triggered());
        assert!(config.days_remaining() < 70);

        // Owner proves liveness — resets the timer
        config.record_proof_of_life();
        assert!(config.days_remaining() > 360);
    }
}
