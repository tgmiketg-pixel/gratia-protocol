//! Vote casting and tallying for governance proposals.
//!
//! Implements the one-phone-one-vote model:
//! - Each verified node (with valid PoL) gets exactly one vote per proposal.
//! - Quorum: 20% of active mining nodes must participate.
//! - Passage: 51% of yes+no votes (abstains count for quorum but not majority).
//! - Emergency voting: 75% supermajority of validator committee.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::config::GovernanceConfig;
use gratia_core::types::{NodeId, Proposal, ProposalStatus, Vote};

use crate::error::GovernanceError;
use crate::proposals::ProposalStore;

/// Record of a single vote cast on a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteRecord {
    pub voter: NodeId,
    pub vote: Vote,
    pub timestamp: DateTime<Utc>,
}

/// Aggregated vote results for a proposal.
#[derive(Debug, Clone)]
pub struct VoteResults {
    pub proposal_id: [u8; 32],
    pub votes_yes: u64,
    pub votes_no: u64,
    pub votes_abstain: u64,
    pub total_votes: u64,
    pub eligible_voters: u64,
    /// Whether 20% quorum of eligible voters has been reached.
    pub quorum_met: bool,
    /// Whether the proposal has passed (51% of yes+no, with quorum).
    pub passed: bool,
}

/// Manages vote records and prevents double voting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VotingManager {
    /// Maps proposal_id -> set of NodeIds that have voted.
    voted: HashMap<[u8; 32], HashSet<NodeId>>,
    /// Full vote records per proposal.
    records: HashMap<[u8; 32], Vec<VoteRecord>>,
}

impl VotingManager {
    pub fn new() -> Self {
        Self {
            voted: HashMap::new(),
            records: HashMap::new(),
        }
    }

    /// Cast a vote on a governance proposal.
    ///
    /// Requirements:
    /// - Proposal must be in Voting phase.
    /// - Voter must have a valid Proof of Life (caller verifies and passes `has_valid_pol`).
    /// - Voter must not have already voted on this proposal.
    pub fn cast_vote(
        &mut self,
        proposal_store: &mut ProposalStore,
        proposal_id: &[u8; 32],
        voter: NodeId,
        vote: Vote,
        has_valid_pol: bool,
        now: DateTime<Utc>,
    ) -> Result<(), GovernanceError> {
        // Verify voter has valid PoL.
        if !has_valid_pol {
            return Err(GovernanceError::NoValidProofOfLife);
        }

        // Verify proposal exists and is in voting phase.
        let proposal = proposal_store
            .get_proposal(proposal_id)
            .ok_or_else(|| GovernanceError::ProposalNotFound {
                id: hex::encode(proposal_id),
            })?;

        if proposal.status != ProposalStatus::Voting {
            return Err(GovernanceError::WrongPhase {
                expected: "Voting".into(),
                actual: format!("{:?}", proposal.status),
            });
        }

        // Prevent double voting.
        let voters = self.voted.entry(*proposal_id).or_default();
        if voters.contains(&voter) {
            return Err(GovernanceError::AlreadyVoted { node_id: voter });
        }

        // Record the vote.
        voters.insert(voter);
        self.records
            .entry(*proposal_id)
            .or_default()
            .push(VoteRecord {
                voter,
                vote: vote.clone(),
                timestamp: now,
            });

        // Update the proposal tally.
        let proposal = proposal_store.get_proposal_mut(proposal_id).unwrap();
        match vote {
            Vote::Yes => proposal.votes_yes += 1,
            Vote::No => proposal.votes_no += 1,
            Vote::Abstain => proposal.votes_abstain += 1,
        }

        Ok(())
    }

    /// Get the current vote results for a proposal.
    pub fn get_vote_results(
        &self,
        proposal: &Proposal,
        _config: &GovernanceConfig,
    ) -> VoteResults {
        let total_votes = proposal.votes_yes.saturating_add(proposal.votes_no).saturating_add(proposal.votes_abstain);

        VoteResults {
            proposal_id: proposal.id,
            votes_yes: proposal.votes_yes,
            votes_no: proposal.votes_no,
            votes_abstain: proposal.votes_abstain,
            total_votes,
            eligible_voters: proposal.eligible_voters,
            quorum_met: proposal.quorum_met(),
            passed: proposal.passed(),
        }
    }

    /// Get the full vote records for a proposal.
    pub fn get_records(&self, proposal_id: &[u8; 32]) -> &[VoteRecord] {
        self.records
            .get(proposal_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Check whether a specific node has voted on a proposal.
    pub fn has_voted(&self, proposal_id: &[u8; 32], node_id: &NodeId) -> bool {
        self.voted
            .get(proposal_id)
            .map(|set| set.contains(node_id))
            .unwrap_or(false)
    }

    /// Get the number of votes cast on a proposal.
    pub fn vote_count(&self, proposal_id: &[u8; 32]) -> usize {
        self.records
            .get(proposal_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

impl Default for VotingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
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

    /// Helper: create a proposal and advance it to the Voting phase.
    fn setup_voting_proposal(
        store: &mut ProposalStore,
        config: &GovernanceConfig,
        eligible_voters: u64,
    ) -> [u8; 32] {
        let ts = now();
        let id = store
            .submit_proposal(
                test_node(1),
                90,
                "Test".into(),
                "Desc".into(),
                vec![],
                eligible_voters,
                config,
                ts,
            )
            .unwrap();

        // Advance past discussion period.
        let after = ts + Duration::seconds(config.discussion_period_secs as i64 + 1);
        store.advance_all(config, after);

        id
    }

    #[test]
    fn test_cast_vote_success() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        let id = setup_voting_proposal(&mut store, &config, 100);
        let voter = test_node(10);

        voting
            .cast_vote(&mut store, &id, voter, Vote::Yes, true, now())
            .unwrap();

        assert!(voting.has_voted(&id, &voter));
        assert_eq!(store.get_proposal(&id).unwrap().votes_yes, 1);
    }

    #[test]
    fn test_double_vote_rejected() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        let id = setup_voting_proposal(&mut store, &config, 100);
        let voter = test_node(10);

        voting
            .cast_vote(&mut store, &id, voter, Vote::Yes, true, now())
            .unwrap();

        let result = voting.cast_vote(&mut store, &id, voter, Vote::No, true, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_vote_without_pol_rejected() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        let id = setup_voting_proposal(&mut store, &config, 100);

        let result = voting.cast_vote(&mut store, &id, test_node(10), Vote::Yes, false, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_vote_during_discussion_rejected() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        // Create proposal but do NOT advance to voting.
        let id = store
            .submit_proposal(
                test_node(1),
                90,
                "T".into(),
                "D".into(),
                vec![],
                100,
                &config,
                now(),
            )
            .unwrap();

        let result = voting.cast_vote(&mut store, &id, test_node(10), Vote::Yes, true, now());
        assert!(result.is_err());
    }

    #[test]
    fn test_quorum_and_passage() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        // 100 eligible voters. Quorum = 20%. Need 20 votes.
        let id = setup_voting_proposal(&mut store, &config, 100);

        // Cast 25 yes, 5 no = 30 total votes. Quorum met (30 >= 20).
        // Passage: 25 / 30 = 83% > 51%. Should pass.
        for i in 0..25u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 10), Vote::Yes, true, now())
                .unwrap();
        }
        for i in 0..5u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 50), Vote::No, true, now())
                .unwrap();
        }

        let results = voting.get_vote_results(store.get_proposal(&id).unwrap(), &config);
        assert!(results.quorum_met);
        assert!(results.passed);
        assert_eq!(results.total_votes, 30);
    }

    #[test]
    fn test_quorum_not_met() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        // 100 eligible voters. Need 20 votes for quorum.
        let id = setup_voting_proposal(&mut store, &config, 100);

        // Only 10 votes — quorum not met.
        for i in 0..10u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 10), Vote::Yes, true, now())
                .unwrap();
        }

        let results = voting.get_vote_results(store.get_proposal(&id).unwrap(), &config);
        assert!(!results.quorum_met);
        assert!(!results.passed);
    }

    #[test]
    fn test_majority_not_met() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        let id = setup_voting_proposal(&mut store, &config, 100);

        // 10 yes, 15 no = 25 total. Quorum met (25 >= 20).
        // Passage: 10 / 25 = 40% < 51%. Should fail.
        for i in 0..10u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 10), Vote::Yes, true, now())
                .unwrap();
        }
        for i in 0..15u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 50), Vote::No, true, now())
                .unwrap();
        }

        let results = voting.get_vote_results(store.get_proposal(&id).unwrap(), &config);
        assert!(results.quorum_met);
        assert!(!results.passed);
    }

    #[test]
    fn test_abstain_counts_for_quorum_not_majority() {
        let config = default_config();
        let mut store = ProposalStore::new();
        let mut voting = VotingManager::new();

        let id = setup_voting_proposal(&mut store, &config, 100);

        // 8 yes, 2 no, 15 abstain = 25 total. Quorum met.
        // Passage: 8 / 10 (yes+no only) = 80% > 51%. Should pass.
        for i in 0..8u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 10), Vote::Yes, true, now())
                .unwrap();
        }
        for i in 0..2u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 30), Vote::No, true, now())
                .unwrap();
        }
        for i in 0..15u8 {
            voting
                .cast_vote(&mut store, &id, test_node(i + 50), Vote::Abstain, true, now())
                .unwrap();
        }

        let results = voting.get_vote_results(store.get_proposal(&id).unwrap(), &config);
        assert!(results.quorum_met);
        assert!(results.passed);
    }
}
