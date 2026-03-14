//! gratia-governance — One-phone-one-vote governance for the Gratia protocol.
//!
//! Implements:
//! - **Standard proposals:** 90+ days PoL history to submit, 14-day discussion,
//!   7-day voting, 51% majority with 20% quorum, 30-day implementation delay.
//! - **Emergency proposals:** 75% supermajority of validator committee, must be
//!   ratified by standard governance vote within 90 days.
//! - **On-chain polling:** Any GRAT holder can create a poll (costs GRAT, burned).
//!   One phone, one vote per poll. Results publicly auditable.
//!
//! NOT token-weighted. One verified node = one vote.

pub mod error;
pub mod polling;
pub mod proposals;
pub mod voting;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::config::GovernanceConfig;
use gratia_core::types::{
    Address, GeoLocation, GeographicFilter, Lux, NodeId, Poll, Proposal, Vote,
};

use crate::error::GovernanceError;
use crate::polling::{PollResults, PollStore};
use crate::proposals::{EmergencyProposal, ProposalPhase, ProposalStore};
use crate::voting::{VoteResults, VotingManager};

/// Central manager for all governance operations.
///
/// Coordinates proposal lifecycle, vote casting, and on-chain polling.
/// Designed to be called from the transaction processing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceManager {
    config: GovernanceConfig,
    proposal_store: ProposalStore,
    voting_manager: VotingManager,
    poll_store: PollStore,
}

impl GovernanceManager {
    pub fn new(config: GovernanceConfig) -> Self {
        Self {
            config,
            proposal_store: ProposalStore::new(),
            voting_manager: VotingManager::new(),
            poll_store: PollStore::new(),
        }
    }

    /// Get the current governance configuration.
    pub fn config(&self) -> &GovernanceConfig {
        &self.config
    }

    /// Update the governance configuration (e.g., via a passed governance proposal).
    /// The governance system itself is subject to governance.
    pub fn update_config(&mut self, config: GovernanceConfig) {
        self.config = config;
    }

    // ========================================================================
    // Proposal operations
    // ========================================================================

    /// Submit a new governance proposal.
    ///
    /// The caller must provide the proposer's PoL history (number of consecutive days).
    pub fn submit_proposal(
        &mut self,
        proposer: NodeId,
        proposer_pol_days: u64,
        title: String,
        description: String,
        proposal_data: Vec<u8>,
        eligible_voters: u64,
        now: DateTime<Utc>,
    ) -> Result<[u8; 32], GovernanceError> {
        self.proposal_store.submit_proposal(
            proposer,
            proposer_pol_days,
            title,
            description,
            proposal_data,
            eligible_voters,
            &self.config,
            now,
        )
    }

    /// Cancel a proposal (only by original proposer, only during discussion).
    pub fn cancel_proposal(
        &mut self,
        proposal_id: &[u8; 32],
        requester: &NodeId,
    ) -> Result<(), GovernanceError> {
        self.proposal_store.cancel_proposal(proposal_id, requester)
    }

    /// Get a proposal by ID.
    pub fn get_proposal(&self, id: &[u8; 32]) -> Option<&Proposal> {
        self.proposal_store.get_proposal(id)
    }

    /// Get the extended phase of a proposal.
    pub fn get_proposal_phase(&self, id: &[u8; 32]) -> Option<ProposalPhase> {
        self.proposal_store.get_phase(id)
    }

    /// Get all active proposals (discussion or voting phase).
    pub fn get_active_proposals(&self) -> Vec<&Proposal> {
        self.proposal_store.active_proposals()
    }

    /// Get all proposals regardless of status.
    pub fn get_all_proposals(&self) -> Vec<&Proposal> {
        self.proposal_store.all_proposals()
    }

    // ========================================================================
    // Voting operations
    // ========================================================================

    /// Cast a vote on a governance proposal.
    ///
    /// One phone, one vote. Voter must have valid PoL.
    pub fn cast_vote(
        &mut self,
        proposal_id: &[u8; 32],
        voter: NodeId,
        vote: Vote,
        has_valid_pol: bool,
        now: DateTime<Utc>,
    ) -> Result<(), GovernanceError> {
        self.voting_manager.cast_vote(
            &mut self.proposal_store,
            proposal_id,
            voter,
            vote,
            has_valid_pol,
            now,
        )
    }

    /// Get current vote results for a proposal.
    pub fn get_vote_results(&self, proposal_id: &[u8; 32]) -> Option<VoteResults> {
        let proposal = self.proposal_store.get_proposal(proposal_id)?;
        Some(
            self.voting_manager
                .get_vote_results(proposal, &self.config),
        )
    }

    /// Check whether a node has voted on a proposal.
    pub fn has_voted(&self, proposal_id: &[u8; 32], node_id: &NodeId) -> bool {
        self.voting_manager.has_voted(proposal_id, node_id)
    }

    // ========================================================================
    // Emergency proposal operations
    // ========================================================================

    /// Submit an emergency security patch proposal.
    pub fn submit_emergency(
        &mut self,
        proposer: NodeId,
        title: String,
        description: String,
        proposal_data: Vec<u8>,
        committee_size: usize,
        now: DateTime<Utc>,
    ) -> [u8; 32] {
        self.proposal_store.submit_emergency(
            proposer,
            title,
            description,
            proposal_data,
            committee_size,
            &self.config,
            now,
        )
    }

    /// A validator committee member approves an emergency proposal.
    /// Returns true if the proposal was just activated (supermajority reached).
    pub fn approve_emergency(
        &mut self,
        emergency_id: &[u8; 32],
        approver: NodeId,
        committee: &[NodeId],
    ) -> Result<bool, GovernanceError> {
        self.proposal_store
            .approve_emergency(emergency_id, approver, committee, &self.config)
    }

    /// Mark an emergency proposal as ratified by standard governance.
    pub fn ratify_emergency(
        &mut self,
        emergency_id: &[u8; 32],
    ) -> Result<(), GovernanceError> {
        self.proposal_store.ratify_emergency(emergency_id)
    }

    /// Get an emergency proposal by ID.
    pub fn get_emergency(&self, id: &[u8; 32]) -> Option<&EmergencyProposal> {
        self.proposal_store.get_emergency(id)
    }

    // ========================================================================
    // Poll operations
    // ========================================================================

    /// Create an on-chain poll. Costs GRAT (burned).
    pub fn create_poll(
        &mut self,
        creator: Address,
        question: String,
        options: Vec<String>,
        duration_secs: u64,
        geographic_filter: Option<GeographicFilter>,
        creator_balance: Lux,
        now: DateTime<Utc>,
    ) -> Result<[u8; 32], GovernanceError> {
        self.poll_store.create_poll(
            creator,
            question,
            options,
            duration_secs,
            geographic_filter,
            creator_balance,
            &self.config,
            now,
        )
    }

    /// Cast a vote on a poll.
    pub fn cast_poll_vote(
        &mut self,
        poll_id: &[u8; 32],
        voter: NodeId,
        option_index: u32,
        has_valid_pol: bool,
        voter_location: Option<GeoLocation>,
        now: DateTime<Utc>,
    ) -> Result<(), GovernanceError> {
        self.poll_store
            .cast_poll_vote(poll_id, voter, option_index, has_valid_pol, voter_location, now)
    }

    /// Get poll results (publicly auditable).
    pub fn get_poll_results(&self, poll_id: &[u8; 32]) -> Option<PollResults> {
        self.poll_store.get_poll_results(poll_id)
    }

    /// Get a poll by ID.
    pub fn get_poll(&self, id: &[u8; 32]) -> Option<&Poll> {
        self.poll_store.get_poll(id)
    }

    /// Get all active (non-expired) polls.
    pub fn get_active_polls(&self, now: DateTime<Utc>) -> Vec<&Poll> {
        self.poll_store.active_polls(now)
    }

    // ========================================================================
    // Lifecycle
    // ========================================================================

    /// Advance all proposal lifecycles based on the current time.
    ///
    /// Should be called periodically (e.g., on each block) to move proposals
    /// through their phases as time windows elapse.
    pub fn tick(&mut self, now: DateTime<Utc>) {
        self.proposal_store.advance_all(&self.config, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use gratia_core::config::GovernanceConfig;
    use gratia_core::types::LUX_PER_GRAT;

    fn test_node(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId(bytes)
    }

    fn test_address(id: u8) -> Address {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        Address(bytes)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn default_config() -> GovernanceConfig {
        GovernanceConfig::default()
    }

    fn manager() -> GovernanceManager {
        GovernanceManager::new(default_config())
    }

    #[test]
    fn test_full_proposal_lifecycle_pass() {
        let mut mgr = manager();
        let ts = now();
        let proposer = test_node(1);
        let config = default_config();

        // Submit proposal.
        let id = mgr
            .submit_proposal(
                proposer,
                90,
                "Increase block size".into(),
                "Proposal to increase block size to 512KB".into(),
                vec![],
                100,
                ts,
            )
            .unwrap();

        assert_eq!(mgr.get_proposal_phase(&id), Some(ProposalPhase::Discussion));

        // Advance past discussion.
        let t1 = ts + Duration::seconds(config.discussion_period_secs as i64 + 1);
        mgr.tick(t1);
        assert_eq!(mgr.get_proposal_phase(&id), Some(ProposalPhase::Voting));

        // Cast votes: 30 yes, 5 no. Quorum = 20/100 = 20. 35 votes > 20.
        for i in 0..30u8 {
            mgr.cast_vote(&id, test_node(i + 10), Vote::Yes, true, t1)
                .unwrap();
        }
        for i in 0..5u8 {
            mgr.cast_vote(&id, test_node(i + 50), Vote::No, true, t1)
                .unwrap();
        }

        let results = mgr.get_vote_results(&id).unwrap();
        assert!(results.quorum_met);
        assert!(results.passed);

        // Advance past voting.
        let t2 = t1 + Duration::seconds(config.voting_period_secs as i64 + 1);
        mgr.tick(t2);
        assert_eq!(mgr.get_proposal_phase(&id), Some(ProposalPhase::Approved));

        // Advance past implementation delay.
        let t3 = t2 + Duration::seconds(config.implementation_delay_secs as i64 + 1);
        mgr.tick(t3);
        assert_eq!(mgr.get_proposal_phase(&id), Some(ProposalPhase::Executed));
    }

    #[test]
    fn test_full_proposal_lifecycle_fail() {
        let mut mgr = manager();
        let ts = now();
        let config = default_config();

        let id = mgr
            .submit_proposal(test_node(1), 90, "T".into(), "D".into(), vec![], 100, ts)
            .unwrap();

        // Advance to voting.
        let t1 = ts + Duration::seconds(config.discussion_period_secs as i64 + 1);
        mgr.tick(t1);

        // Cast 10 yes, 15 no. Quorum met (25 >= 20), but majority fails (10/25 < 51%).
        for i in 0..10u8 {
            mgr.cast_vote(&id, test_node(i + 10), Vote::Yes, true, t1)
                .unwrap();
        }
        for i in 0..15u8 {
            mgr.cast_vote(&id, test_node(i + 50), Vote::No, true, t1)
                .unwrap();
        }

        // Advance past voting.
        let t2 = t1 + Duration::seconds(config.voting_period_secs as i64 + 1);
        mgr.tick(t2);
        assert_eq!(mgr.get_proposal_phase(&id), Some(ProposalPhase::Rejected));
    }

    #[test]
    fn test_poll_through_manager() {
        let mut mgr = manager();
        let ts = now();
        let creator = test_address(1);

        let id = mgr
            .create_poll(
                creator,
                "Best fruit?".into(),
                vec!["Apple".into(), "Banana".into()],
                86400,
                None,
                100 * LUX_PER_GRAT,
                ts,
            )
            .unwrap();

        mgr.cast_poll_vote(&id, test_node(10), 0, true, None, ts)
            .unwrap();
        mgr.cast_poll_vote(&id, test_node(11), 1, true, None, ts)
            .unwrap();
        mgr.cast_poll_vote(&id, test_node(12), 0, true, None, ts)
            .unwrap();

        let results = mgr.get_poll_results(&id).unwrap();
        assert_eq!(results.total_voters, 3);
        assert_eq!(results.options[0].votes, 2);
        assert_eq!(results.options[1].votes, 1);
    }

    #[test]
    fn test_emergency_through_manager() {
        let mut mgr = manager();
        let ts = now();

        let committee: Vec<NodeId> = (0..21).map(|i| test_node(i)).collect();

        let id = mgr.submit_emergency(
            test_node(0),
            "Critical patch".into(),
            "Fix vulnerability".into(),
            vec![],
            21,
            ts,
        );

        // Approve with 16/21 committee members (>= 75%).
        for i in 0..15 {
            let activated = mgr.approve_emergency(&id, committee[i], &committee).unwrap();
            assert!(!activated);
        }

        let activated = mgr.approve_emergency(&id, committee[15], &committee).unwrap();
        assert!(activated);

        // Ratify it.
        mgr.ratify_emergency(&id).unwrap();
        assert!(mgr.get_emergency(&id).unwrap().ratified);
    }

    #[test]
    fn test_governance_config_update() {
        let mut mgr = manager();
        assert_eq!(mgr.config().min_proposer_history_days, 90);

        let mut new_config = default_config();
        new_config.min_proposer_history_days = 120;
        mgr.update_config(new_config);

        assert_eq!(mgr.config().min_proposer_history_days, 120);
    }

    #[test]
    fn test_active_proposals_filter() {
        let mut mgr = manager();
        let ts = now();
        let config = default_config();

        // Submit two proposals.
        let id1 = mgr
            .submit_proposal(test_node(1), 90, "P1".into(), "D1".into(), vec![], 100, ts)
            .unwrap();
        let _id2 = mgr
            .submit_proposal(test_node(2), 90, "P2".into(), "D2".into(), vec![], 100, ts)
            .unwrap();

        assert_eq!(mgr.get_active_proposals().len(), 2);

        // Cancel first proposal.
        mgr.cancel_proposal(&id1, &test_node(1)).unwrap();

        // Only P2 should be active.
        let active = mgr.get_active_proposals();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].title, "P2");
    }
}
