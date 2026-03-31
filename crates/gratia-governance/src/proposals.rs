//! Proposal submission and lifecycle management.
//!
//! Lifecycle: Draft -> Discussion (14 days) -> Voting (7 days) -> Passed/Failed
//!            -> Implementation (30-day delay) -> Executed
//!
//! Emergency proposals follow a separate fast-track path requiring 75%
//! supermajority of the validator committee, with mandatory ratification
//! by standard vote within 90 days.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use gratia_core::config::GovernanceConfig;
use gratia_core::types::{NodeId, Proposal, ProposalStatus};

use crate::error::GovernanceError;

/// Extended lifecycle status that includes Draft and Cancelled states
/// not present in the core ProposalStatus enum. We map to core status
/// for storage in the Proposal struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalPhase {
    /// Author is still editing; not yet submitted publicly.
    Draft,
    /// Public discussion period (14 days by default).
    Discussion,
    /// Voting period (7 days by default).
    Voting,
    /// Passed, waiting for implementation delay (30 days).
    Approved,
    /// Failed to reach quorum or majority.
    Rejected,
    /// Implementation delay elapsed; proposal is now active.
    Executed,
    /// Cancelled by the proposer during discussion.
    Cancelled,
}

impl ProposalPhase {
    /// Convert to the core ProposalStatus for the Proposal struct.
    pub fn to_core_status(self) -> ProposalStatus {
        match self {
            ProposalPhase::Draft => ProposalStatus::Discussion,
            ProposalPhase::Discussion => ProposalStatus::Discussion,
            ProposalPhase::Voting => ProposalStatus::Voting,
            ProposalPhase::Approved => ProposalStatus::Approved,
            ProposalPhase::Rejected => ProposalStatus::Rejected,
            ProposalPhase::Executed => ProposalStatus::Implemented,
            ProposalPhase::Cancelled => ProposalStatus::Rejected,
        }
    }
}

/// An emergency security patch proposal.
///
/// Follows a fast-track path: 75% supermajority of the 21-member validator
/// committee to activate immediately. Must be ratified by standard governance
/// vote within 90 days or it is automatically reverted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmergencyProposal {
    pub id: [u8; 32],
    pub proposer: NodeId,
    pub title: String,
    pub description: String,
    pub proposal_data: Vec<u8>,
    pub submitted_at: DateTime<Utc>,
    /// Deadline for ratification by standard governance vote.
    pub ratification_deadline: DateTime<Utc>,
    /// Committee members who approved.
    pub approvals: Vec<NodeId>,
    /// Total committee size at time of creation.
    pub committee_size: usize,
    /// Whether the emergency action has been activated.
    pub activated: bool,
    /// Whether it has been ratified by a standard governance vote.
    pub ratified: bool,
    /// Whether it was reverted (ratification deadline passed without ratification).
    pub reverted: bool,
}

impl EmergencyProposal {
    /// Check whether enough committee members have approved to activate.
    /// Requires 75% supermajority.
    pub fn has_supermajority(&self, threshold_bps: u32) -> bool {
        if self.committee_size == 0 {
            return false;
        }
        // threshold_bps is e.g. 7500 for 75%
        let required = (self.committee_size as u64).saturating_mul(threshold_bps as u64).saturating_add(9999) / 10000;
        self.approvals.len() as u64 >= required
    }
}

/// Manages all proposals (standard and emergency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalStore {
    /// Standard proposals indexed by ID.
    proposals: HashMap<[u8; 32], Proposal>,
    /// Extended phase tracking (the core Proposal.status is synced).
    phases: HashMap<[u8; 32], ProposalPhase>,
    /// Emergency proposals indexed by ID.
    emergency_proposals: HashMap<[u8; 32], EmergencyProposal>,
    /// Counter used as part of proposal ID generation.
    next_proposal_nonce: u64,
}

impl ProposalStore {
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            phases: HashMap::new(),
            emergency_proposals: HashMap::new(),
            next_proposal_nonce: 0,
        }
    }

    /// Submit a new standard proposal.
    ///
    /// The proposer must have at least `min_proposer_history_days` consecutive
    /// days of valid Proof of Life. The caller is responsible for verifying this
    /// and passing the actual count.
    pub fn submit_proposal(
        &mut self,
        proposer: NodeId,
        proposer_pol_days: u64,
        title: String,
        description: String,
        proposal_data: Vec<u8>,
        eligible_voters: u64,
        config: &GovernanceConfig,
        now: DateTime<Utc>,
    ) -> Result<[u8; 32], GovernanceError> {
        // Enforce 90+ days PoL history.
        if proposer_pol_days < config.min_proposer_history_days {
            return Err(GovernanceError::InsufficientHistory {
                days: proposer_pol_days,
                required: config.min_proposer_history_days,
            });
        }

        let id = self.generate_id(&proposer, now);

        let discussion_ends = now + Duration::seconds(config.discussion_period_secs as i64);
        let voting_ends = discussion_ends + Duration::seconds(config.voting_period_secs as i64);
        let implementation_date =
            voting_ends + Duration::seconds(config.implementation_delay_secs as i64);

        let proposal = Proposal {
            id,
            proposer,
            title,
            description,
            proposal_data,
            submitted_at: now,
            discussion_ends,
            voting_ends,
            implementation_date,
            status: ProposalStatus::Discussion,
            votes_yes: 0,
            votes_no: 0,
            votes_abstain: 0,
            eligible_voters,
        };

        self.proposals.insert(id, proposal);
        self.phases.insert(id, ProposalPhase::Discussion);

        tracing::info!(
            proposal_id = hex::encode(id),
            proposer = %proposer,
            "new governance proposal submitted"
        );

        Ok(id)
    }

    /// Cancel a proposal. Only the original proposer can cancel, and only
    /// during the discussion phase.
    pub fn cancel_proposal(
        &mut self,
        proposal_id: &[u8; 32],
        requester: &NodeId,
    ) -> Result<(), GovernanceError> {
        let proposal = self.proposals.get(proposal_id).ok_or_else(|| {
            GovernanceError::ProposalNotFound {
                id: hex::encode(proposal_id),
            }
        })?;

        if &proposal.proposer != requester {
            return Err(GovernanceError::NotProposer);
        }

        let phase = self.phases.get(proposal_id).copied().unwrap_or(ProposalPhase::Discussion);
        if phase != ProposalPhase::Discussion {
            return Err(GovernanceError::CannotCancel);
        }

        // Update phase and core status.
        self.phases.insert(*proposal_id, ProposalPhase::Cancelled);
        if let Some(p) = self.proposals.get_mut(proposal_id) {
            p.status = ProposalStatus::Rejected;
        }

        tracing::info!(
            proposal_id = hex::encode(proposal_id),
            "proposal cancelled by proposer"
        );

        Ok(())
    }

    /// Advance all proposals through their lifecycle based on the current time.
    ///
    /// Moves proposals from Discussion -> Voting -> Approved/Rejected -> Executed
    /// as their time windows elapse.
    pub fn advance_all(&mut self, config: &GovernanceConfig, now: DateTime<Utc>) {
        let ids: Vec<[u8; 32]> = self.proposals.keys().copied().collect();

        for id in ids {
            self.advance_proposal(&id, config, now);
        }

        // Also check emergency proposal ratification deadlines.
        let emergency_ids: Vec<[u8; 32]> = self.emergency_proposals.keys().copied().collect();
        for id in emergency_ids {
            if let Some(ep) = self.emergency_proposals.get_mut(&id) {
                // WHY: Auto-revert emergency proposals that were activated but not ratified
                // before the deadline. This prevents the validator committee from making
                // permanent changes without community approval.
                if ep.activated && !ep.ratified && !ep.reverted && now >= ep.ratification_deadline {
                    ep.reverted = true;
                    tracing::warn!(
                        emergency_id = hex::encode(id),
                        "emergency proposal auto-reverted: ratification deadline passed"
                    );
                }
            }
        }
    }

    /// Advance a single proposal through its lifecycle.
    fn advance_proposal(
        &mut self,
        proposal_id: &[u8; 32],
        _config: &GovernanceConfig,
        now: DateTime<Utc>,
    ) {
        let phase = match self.phases.get(proposal_id) {
            Some(p) => *p,
            None => return,
        };

        let proposal = match self.proposals.get_mut(proposal_id) {
            Some(p) => p,
            None => return,
        };

        match phase {
            ProposalPhase::Discussion => {
                if now >= proposal.discussion_ends {
                    self.phases.insert(*proposal_id, ProposalPhase::Voting);
                    proposal.status = ProposalStatus::Voting;
                    tracing::info!(
                        proposal_id = hex::encode(proposal_id),
                        "proposal moved to voting phase"
                    );
                }
            }
            ProposalPhase::Voting => {
                if now >= proposal.voting_ends {
                    if proposal.passed() {
                        self.phases.insert(*proposal_id, ProposalPhase::Approved);
                        proposal.status = ProposalStatus::Approved;
                        tracing::info!(
                            proposal_id = hex::encode(proposal_id),
                            yes = proposal.votes_yes,
                            no = proposal.votes_no,
                            abstain = proposal.votes_abstain,
                            "proposal approved"
                        );
                    } else {
                        self.phases.insert(*proposal_id, ProposalPhase::Rejected);
                        proposal.status = ProposalStatus::Rejected;
                        tracing::info!(
                            proposal_id = hex::encode(proposal_id),
                            yes = proposal.votes_yes,
                            no = proposal.votes_no,
                            abstain = proposal.votes_abstain,
                            quorum_met = proposal.quorum_met(),
                            "proposal rejected"
                        );
                    }
                }
            }
            ProposalPhase::Approved => {
                if now >= proposal.implementation_date {
                    self.phases.insert(*proposal_id, ProposalPhase::Executed);
                    proposal.status = ProposalStatus::Implemented;
                    tracing::info!(
                        proposal_id = hex::encode(proposal_id),
                        "proposal executed after implementation delay"
                    );
                }
            }
            // Terminal states — nothing to advance.
            ProposalPhase::Draft
            | ProposalPhase::Rejected
            | ProposalPhase::Executed
            | ProposalPhase::Cancelled => {}
        }
    }

    /// Submit an emergency security patch proposal.
    pub fn submit_emergency(
        &mut self,
        proposer: NodeId,
        title: String,
        description: String,
        proposal_data: Vec<u8>,
        committee_size: usize,
        config: &GovernanceConfig,
        now: DateTime<Utc>,
    ) -> [u8; 32] {
        let id = self.generate_id(&proposer, now);

        let ratification_deadline =
            now + Duration::seconds(config.emergency_ratification_secs as i64);

        let ep = EmergencyProposal {
            id,
            proposer,
            title,
            description,
            proposal_data,
            submitted_at: now,
            ratification_deadline,
            approvals: Vec::new(),
            committee_size,
            activated: false,
            ratified: false,
            reverted: false,
        };

        self.emergency_proposals.insert(id, ep);

        tracing::info!(
            emergency_id = hex::encode(id),
            proposer = %proposer,
            "emergency proposal submitted"
        );

        id
    }

    /// A validator committee member approves an emergency proposal.
    /// Returns true if the proposal just reached supermajority and was activated.
    pub fn approve_emergency(
        &mut self,
        emergency_id: &[u8; 32],
        approver: NodeId,
        committee: &[NodeId],
        config: &GovernanceConfig,
    ) -> Result<bool, GovernanceError> {
        if !committee.contains(&approver) {
            return Err(GovernanceError::NotOnCommittee);
        }

        let ep = self
            .emergency_proposals
            .get_mut(emergency_id)
            .ok_or_else(|| GovernanceError::EmergencyProposalNotFound {
                id: hex::encode(emergency_id),
            })?;

        if ep.activated || ep.reverted {
            return Err(GovernanceError::EmergencyAlreadyResolved);
        }

        // Prevent double approval.
        if ep.approvals.contains(&approver) {
            return Ok(false);
        }

        ep.approvals.push(approver);

        if !ep.activated && ep.has_supermajority(config.emergency_threshold_bps) {
            ep.activated = true;
            tracing::warn!(
                emergency_id = hex::encode(emergency_id),
                approvals = ep.approvals.len(),
                committee_size = ep.committee_size,
                "emergency proposal ACTIVATED — must be ratified within deadline"
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Mark an emergency proposal as ratified by a standard governance vote.
    pub fn ratify_emergency(
        &mut self,
        emergency_id: &[u8; 32],
    ) -> Result<(), GovernanceError> {
        let ep = self
            .emergency_proposals
            .get_mut(emergency_id)
            .ok_or_else(|| GovernanceError::EmergencyProposalNotFound {
                id: hex::encode(emergency_id),
            })?;

        if !ep.activated || ep.reverted {
            return Err(GovernanceError::EmergencyAlreadyResolved);
        }

        ep.ratified = true;

        tracing::info!(
            emergency_id = hex::encode(emergency_id),
            "emergency proposal ratified by standard governance vote"
        );

        Ok(())
    }

    // -- Accessors --

    pub fn get_proposal(&self, id: &[u8; 32]) -> Option<&Proposal> {
        self.proposals.get(id)
    }

    pub fn get_phase(&self, id: &[u8; 32]) -> Option<ProposalPhase> {
        self.phases.get(id).copied()
    }

    pub fn get_proposal_mut(&mut self, id: &[u8; 32]) -> Option<&mut Proposal> {
        self.proposals.get_mut(id)
    }

    pub fn get_emergency(&self, id: &[u8; 32]) -> Option<&EmergencyProposal> {
        self.emergency_proposals.get(id)
    }

    /// Return all proposals currently in the discussion or voting phase.
    pub fn active_proposals(&self) -> Vec<&Proposal> {
        self.proposals
            .values()
            .filter(|p| {
                matches!(
                    p.status,
                    ProposalStatus::Discussion | ProposalStatus::Voting
                )
            })
            .collect()
    }

    /// Return all proposals (all statuses).
    pub fn all_proposals(&self) -> Vec<&Proposal> {
        self.proposals.values().collect()
    }

    /// Generate a deterministic proposal ID from proposer + timestamp + nonce.
    fn generate_id(&mut self, proposer: &NodeId, now: DateTime<Utc>) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-proposal-v1:");
        hasher.update(proposer.0);
        hasher.update(now.timestamp().to_le_bytes());
        hasher.update(self.next_proposal_nonce.to_le_bytes());
        self.next_proposal_nonce += 1;

        let result = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&result);
        id
    }
}

impl Default for ProposalStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::config::GovernanceConfig;

    fn test_node(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId(bytes)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn default_config() -> GovernanceConfig {
        GovernanceConfig::default()
    }

    #[test]
    fn test_submit_proposal_success() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let proposer = test_node(1);

        let id = store
            .submit_proposal(
                proposer,
                90,
                "Test Proposal".into(),
                "A test".into(),
                vec![],
                1000,
                &config,
                now(),
            )
            .unwrap();

        let proposal = store.get_proposal(&id).unwrap();
        assert_eq!(proposal.proposer, proposer);
        assert_eq!(proposal.status, ProposalStatus::Discussion);
        assert_eq!(store.get_phase(&id), Some(ProposalPhase::Discussion));
    }

    #[test]
    fn test_submit_proposal_insufficient_history() {
        let mut store = ProposalStore::new();
        let config = default_config();

        let result = store.submit_proposal(
            test_node(1),
            89, // One day short.
            "Test".into(),
            "Desc".into(),
            vec![],
            1000,
            &config,
            now(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_cancel_proposal() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let proposer = test_node(1);

        let id = store
            .submit_proposal(proposer, 90, "T".into(), "D".into(), vec![], 1000, &config, now())
            .unwrap();

        store.cancel_proposal(&id, &proposer).unwrap();
        assert_eq!(store.get_phase(&id), Some(ProposalPhase::Cancelled));
    }

    #[test]
    fn test_cancel_by_non_proposer_fails() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let proposer = test_node(1);
        let other = test_node(2);

        let id = store
            .submit_proposal(proposer, 90, "T".into(), "D".into(), vec![], 1000, &config, now())
            .unwrap();

        let result = store.cancel_proposal(&id, &other);
        assert!(result.is_err());
    }

    #[test]
    fn test_advance_discussion_to_voting() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .submit_proposal(test_node(1), 90, "T".into(), "D".into(), vec![], 1000, &config, ts)
            .unwrap();

        // Advance time past discussion period (14 days).
        let after = ts + Duration::seconds(config.discussion_period_secs as i64 + 1);
        store.advance_all(&config, after);

        assert_eq!(store.get_phase(&id), Some(ProposalPhase::Voting));
        assert_eq!(
            store.get_proposal(&id).unwrap().status,
            ProposalStatus::Voting
        );
    }

    #[test]
    fn test_advance_voting_to_rejected_no_votes() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .submit_proposal(test_node(1), 90, "T".into(), "D".into(), vec![], 1000, &config, ts)
            .unwrap();

        // Move past discussion + voting periods.
        let after_discussion = ts + Duration::seconds(config.discussion_period_secs as i64 + 1);
        store.advance_all(&config, after_discussion);

        let after_voting = after_discussion + Duration::seconds(config.voting_period_secs as i64 + 1);
        store.advance_all(&config, after_voting);

        // No votes = no quorum = rejected.
        assert_eq!(store.get_phase(&id), Some(ProposalPhase::Rejected));
    }

    #[test]
    fn test_advance_approved_to_executed() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let ts = now();

        let id = store
            .submit_proposal(test_node(1), 90, "T".into(), "D".into(), vec![], 100, &config, ts)
            .unwrap();

        // Move to voting.
        let t1 = ts + Duration::seconds(config.discussion_period_secs as i64 + 1);
        store.advance_all(&config, t1);

        // Add enough votes to pass (51% with 20% quorum).
        // 100 eligible voters. Quorum = 20. Need >50% yes of yes+no.
        {
            let p = store.get_proposal_mut(&id).unwrap();
            p.votes_yes = 25;
            p.votes_no = 5;
            p.votes_abstain = 0;
        }

        // Move past voting.
        let t2 = t1 + Duration::seconds(config.voting_period_secs as i64 + 1);
        store.advance_all(&config, t2);
        assert_eq!(store.get_phase(&id), Some(ProposalPhase::Approved));

        // Move past implementation delay.
        let t3 = t2 + Duration::seconds(config.implementation_delay_secs as i64 + 1);
        store.advance_all(&config, t3);
        assert_eq!(store.get_phase(&id), Some(ProposalPhase::Executed));
    }

    #[test]
    fn test_emergency_proposal_supermajority() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let ts = now();

        // 21-member committee.
        let committee: Vec<NodeId> = (0..21).map(|i| test_node(i)).collect();

        let id = store.submit_emergency(
            test_node(0),
            "Security Patch".into(),
            "Critical fix".into(),
            vec![1, 2, 3],
            21,
            &config,
            ts,
        );

        // 75% of 21 = ceil(15.75) = 16 approvals needed.
        for i in 0..15 {
            let activated = store
                .approve_emergency(&id, committee[i], &committee, &config)
                .unwrap();
            assert!(!activated);
        }

        // 16th approval should activate.
        let activated = store
            .approve_emergency(&id, committee[15], &committee, &config)
            .unwrap();
        assert!(activated);
        assert!(store.get_emergency(&id).unwrap().activated);
    }

    #[test]
    fn test_emergency_auto_revert_on_deadline() {
        let mut store = ProposalStore::new();
        let config = default_config();
        let ts = now();

        let committee: Vec<NodeId> = (0..21).map(|i| test_node(i)).collect();

        let id = store.submit_emergency(
            test_node(0),
            "Patch".into(),
            "Fix".into(),
            vec![],
            21,
            &config,
            ts,
        );

        // Activate it.
        for i in 0..16 {
            store
                .approve_emergency(&id, committee[i], &committee, &config)
                .unwrap();
        }
        assert!(store.get_emergency(&id).unwrap().activated);

        // Advance past ratification deadline without ratifying.
        let after_deadline =
            ts + Duration::seconds(config.emergency_ratification_secs as i64 + 1);
        store.advance_all(&config, after_deadline);

        assert!(store.get_emergency(&id).unwrap().reverted);
    }
}
