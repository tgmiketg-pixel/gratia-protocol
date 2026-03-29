//! Jury-based moderation system for Lux.
//!
//! When a post is reported, a jury of 7 random PoL-verified nodes is selected
//! via VRF. Jurors vote guilty/not guilty. 5/7 to remove, 6/7 to temp-ban.
//! Appeals go to a jury of 11 (requires 8/11).

use crate::types::*;
use chrono::Utc;
use std::collections::HashMap;
use tracing::info;

/// Jury size for standard moderation.
///
/// WHY: 7 jurors balances speed (fewer people to coordinate) with fairness
/// (odd number prevents ties, 5/7 threshold is ~71% agreement).
pub const STANDARD_JURY_SIZE: usize = 7;

/// Votes needed to remove content (standard jury).
pub const VOTES_TO_REMOVE: usize = 5;

/// Votes needed to temp-ban the author (standard jury).
pub const VOTES_TO_BAN: usize = 6;

/// Jury size for appeals.
pub const APPEAL_JURY_SIZE: usize = 11;

/// Votes needed to uphold on appeal.
pub const APPEAL_VOTES_TO_UPHOLD: usize = 8;

/// Temp bans in 30 days before escalation to governance.
///
/// WHY: Three strikes in a month means the community has repeatedly found
/// this user's content unacceptable. Governance handles permanent action.
pub const BANS_BEFORE_ESCALATION: u32 = 3;

/// Duration of a temporary ban in hours.
pub const TEMP_BAN_HOURS: u32 = 24;

/// An active moderation case.
#[derive(Debug, Clone)]
pub struct ModerationCase {
    /// The report that triggered this case.
    pub report: Report,

    /// Selected jury members (wallet addresses).
    pub jury: Vec<String>,

    /// Votes received so far: (juror_address, guilty).
    pub votes: HashMap<String, bool>,

    /// Whether the case has been resolved.
    pub resolved: bool,

    /// The verdict, once resolved.
    pub verdict: Option<Verdict>,

    /// Whether this is an appeal case.
    pub is_appeal: bool,
}

/// The jury-based moderation system.
pub struct JurySystem {
    /// Active cases indexed by reported post hash.
    cases: HashMap<String, ModerationCase>,

    /// Completed verdicts for audit trail.
    verdict_history: Vec<Verdict>,
}

impl JurySystem {
    pub fn new() -> Self {
        Self {
            cases: HashMap::new(),
            verdict_history: Vec::new(),
        }
    }

    /// File a report against a post. Returns the case if jury selection succeeds.
    ///
    /// In production, jury selection uses VRF with the epoch seed to randomly
    /// choose from PoL-verified nodes. For V1, the caller provides the jury
    /// (selected by the consensus layer).
    pub fn file_report(
        &mut self,
        report: Report,
        selected_jury: Vec<String>,
    ) -> Option<&ModerationCase> {
        let post_hash = report.post_hash.clone();

        // Don't create duplicate cases for the same post
        if self.cases.contains_key(&post_hash) {
            return self.cases.get(&post_hash);
        }

        let jury_size = if false { APPEAL_JURY_SIZE } else { STANDARD_JURY_SIZE };
        if selected_jury.len() < jury_size {
            tracing::warn!(
                needed = jury_size,
                got = selected_jury.len(),
                "Not enough jurors available"
            );
            return None;
        }

        let case = ModerationCase {
            report,
            jury: selected_jury[..jury_size].to_vec(),
            votes: HashMap::new(),
            resolved: false,
            verdict: None,
            is_appeal: false,
        };

        self.cases.insert(post_hash.clone(), case);
        info!(post_hash = %post_hash, "Moderation case opened");
        self.cases.get(&post_hash)
    }

    /// Cast a vote on a moderation case.
    ///
    /// Returns the verdict if the case is now resolved.
    pub fn cast_vote(
        &mut self,
        post_hash: &str,
        juror: &str,
        guilty: bool,
    ) -> Option<Verdict> {
        let case = match self.cases.get_mut(post_hash) {
            Some(c) => c,
            None => return None,
        };

        if case.resolved {
            return case.verdict.clone();
        }

        // Only selected jurors can vote
        if !case.jury.contains(&juror.to_string()) {
            tracing::warn!(juror = %juror, "Non-juror attempted to vote");
            return None;
        }

        case.votes.insert(juror.to_string(), guilty);

        // Check if all jurors have voted
        if case.votes.len() >= case.jury.len() {
            let verdict = self.resolve_case(post_hash);
            return verdict;
        }

        None
    }

    /// Resolve a case once all votes are in.
    fn resolve_case(&mut self, post_hash: &str) -> Option<Verdict> {
        let case = self.cases.get_mut(post_hash)?;

        let guilty_count = case.votes.values().filter(|&&v| v).count();
        let jury_size = case.jury.len();

        let (votes_to_remove, votes_to_ban) = if case.is_appeal {
            (APPEAL_VOTES_TO_UPHOLD, APPEAL_VOTES_TO_UPHOLD)
        } else {
            (VOTES_TO_REMOVE, VOTES_TO_BAN)
        };

        let outcome = if guilty_count >= votes_to_ban {
            VerdictOutcome::TempBan { duration_hours: TEMP_BAN_HOURS }
        } else if guilty_count >= votes_to_remove {
            VerdictOutcome::ContentRemoved
        } else {
            VerdictOutcome::NotGuilty
        };

        let verdict = Verdict {
            post_hash: post_hash.to_string(),
            jurors: case.jury.clone(),
            votes: case.jury.iter()
                .map(|j| *case.votes.get(j).unwrap_or(&false))
                .collect(),
            outcome: outcome.clone(),
            block_height: 0, // Set when anchored on-chain
            timestamp: Utc::now(),
        };

        case.resolved = true;
        case.verdict = Some(verdict.clone());
        self.verdict_history.push(verdict.clone());

        info!(
            post_hash = %post_hash,
            guilty = guilty_count,
            total = jury_size,
            outcome = ?outcome,
            "Moderation case resolved"
        );

        Some(verdict)
    }

    /// File an appeal against an existing verdict.
    /// Requires a new, larger jury.
    pub fn file_appeal(
        &mut self,
        post_hash: &str,
        selected_jury: Vec<String>,
    ) -> bool {
        // Must have an existing guilty verdict to appeal
        let existing = match self.cases.get(post_hash) {
            Some(c) if c.resolved => c,
            _ => return false,
        };

        let existing_outcome = existing.verdict.as_ref().map(|v| &v.outcome);
        if existing_outcome == Some(&VerdictOutcome::NotGuilty) {
            return false; // Can't appeal a not-guilty verdict
        }

        if selected_jury.len() < APPEAL_JURY_SIZE {
            return false;
        }

        // Remove old case, create appeal case
        let report = existing.report.clone();
        self.cases.remove(post_hash);

        let case = ModerationCase {
            report,
            jury: selected_jury[..APPEAL_JURY_SIZE].to_vec(),
            votes: HashMap::new(),
            resolved: false,
            verdict: None,
            is_appeal: true,
        };

        self.cases.insert(post_hash.to_string(), case);
        info!(post_hash = %post_hash, "Appeal case opened with jury of {}", APPEAL_JURY_SIZE);
        true
    }

    /// Check if a post has an active (unresolved) moderation case.
    pub fn has_active_case(&self, post_hash: &str) -> bool {
        self.cases.get(post_hash).map_or(false, |c| !c.resolved)
    }

    /// Get the verdict for a post, if one exists.
    pub fn get_verdict(&self, post_hash: &str) -> Option<&Verdict> {
        self.cases.get(post_hash)?.verdict.as_ref()
    }

    /// Get all verdict history for audit purposes.
    pub fn verdict_history(&self) -> &[Verdict] {
        &self.verdict_history
    }

    /// Get active case count.
    pub fn active_case_count(&self) -> usize {
        self.cases.values().filter(|c| !c.resolved).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_report(post_hash: &str) -> Report {
        Report {
            post_hash: post_hash.to_string(),
            reporter: "grat:reporter1".to_string(),
            reason: ReportReason::Spam,
            context: None,
            signature: vec![],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_unanimous_guilty() {
        let mut jury_system = JurySystem::new();
        let jury: Vec<String> = (0..7).map(|i| format!("grat:juror{i}")).collect();

        jury_system.file_report(make_report("post1"), jury.clone());

        for (i, juror) in jury.iter().enumerate() {
            let result = jury_system.cast_vote("post1", juror, true);
            if i < 6 {
                assert!(result.is_none()); // Not all votes in yet
            } else {
                let verdict = result.unwrap();
                assert_eq!(verdict.outcome, VerdictOutcome::TempBan { duration_hours: 24 });
            }
        }
    }

    #[test]
    fn test_not_guilty() {
        let mut jury_system = JurySystem::new();
        let jury: Vec<String> = (0..7).map(|i| format!("grat:juror{i}")).collect();

        jury_system.file_report(make_report("post2"), jury.clone());

        // 4 guilty, 3 not guilty — below threshold
        for (i, juror) in jury.iter().enumerate() {
            jury_system.cast_vote("post2", juror, i < 4);
        }

        let verdict = jury_system.get_verdict("post2").unwrap();
        assert_eq!(verdict.outcome, VerdictOutcome::NotGuilty);
    }

    #[test]
    fn test_content_removed_not_banned() {
        let mut jury_system = JurySystem::new();
        let jury: Vec<String> = (0..7).map(|i| format!("grat:juror{i}")).collect();

        jury_system.file_report(make_report("post3"), jury.clone());

        // 5 guilty, 2 not — enough to remove but not ban
        for (i, juror) in jury.iter().enumerate() {
            jury_system.cast_vote("post3", juror, i < 5);
        }

        let verdict = jury_system.get_verdict("post3").unwrap();
        assert_eq!(verdict.outcome, VerdictOutcome::ContentRemoved);
    }
}
