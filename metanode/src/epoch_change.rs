// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Result, ensure};
use consensus_config::{
    AuthorityIndex, Committee, ProtocolKeyPair, ProtocolKeySignature, Stake,
};
use fastcrypto::hash::{HashFunction, Blake2b256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use std::fmt;
use tracing::{info, error};
use thiserror::Error;
use bcs;

/// Error types for epoch change operations
#[derive(Debug, Error)]
#[allow(dead_code)] // Error variants will be used when implementing full epoch transition
pub enum EpochChangeError {
    #[error("Proposal validation failed: {0}")]
    InvalidProposal(String),
    #[error("Vote validation failed: {0}")]
    InvalidVote(String),
    #[error("Clock drift too large: {0}ms")]
    ClockDriftTooLarge(u64),
    #[error("Transition failed: {0}")]
    TransitionFailed(String),
    #[error("Quorum not reached")]
    QuorumNotReached,
    #[error("Proposal timeout after {0}s")]
    ProposalTimeout(u64),
    #[error("Duplicate proposal detected")]
    DuplicateProposal,
    #[error("Invalid committee: {0}")]
    InvalidCommittee(String),
}

/// Proposal for epoch change
/// NOTE: Using derive Serialize/Deserialize for BCS compatibility
/// BCS requires tuple-style serialization (no field names)
#[derive(Serialize, Deserialize)]
pub struct EpochChangeProposal {
    pub new_epoch: u64,
    pub new_committee: Committee,
    pub new_epoch_timestamp_ms: u64,
    pub proposal_commit_index: u32,
    pub proposer_value: u32,  // Store as u32 for BCS compatibility
    pub signature_bytes: Vec<u8>,  // Store as bytes for BCS compatibility
    /// Timestamp when proposal was created (for timeout checking)
    pub created_at_seconds: u64,
}

impl EpochChangeProposal {
    /// Create new proposal with AuthorityIndex and ProtocolKeySignature
    pub fn new_with_signature(
        new_epoch: u64,
        new_committee: Committee,
        new_epoch_timestamp_ms: u64,
        proposal_commit_index: u32,
        proposer: AuthorityIndex,
        signature: ProtocolKeySignature,
        created_at_seconds: u64,
    ) -> Self {
        Self {
            new_epoch,
            new_committee,
            new_epoch_timestamp_ms,
            proposal_commit_index,
            proposer_value: proposer.value() as u32,
            signature_bytes: signature.to_bytes().to_vec(),
            created_at_seconds,
        }
    }
    
    /// Get proposer as AuthorityIndex
    pub fn proposer(&self) -> AuthorityIndex {
        AuthorityIndex::new_for_test(self.proposer_value)
    }
    
    /// Get signature as ProtocolKeySignature
    pub fn signature(&self) -> Result<ProtocolKeySignature> {
        ProtocolKeySignature::from_bytes(&self.signature_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize signature: {}", e))
    }
}

impl Clone for EpochChangeProposal {
    fn clone(&self) -> Self {
        Self {
            new_epoch: self.new_epoch,
            new_committee: self.new_committee.clone(),
            new_epoch_timestamp_ms: self.new_epoch_timestamp_ms,
            proposal_commit_index: self.proposal_commit_index,
            proposer_value: self.proposer_value,
            signature_bytes: self.signature_bytes.clone(),
            created_at_seconds: self.created_at_seconds,
        }
    }
}

impl fmt::Debug for EpochChangeProposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EpochChangeProposal")
            .field("new_epoch", &self.new_epoch)
            .field("new_committee", &self.new_committee)
            .field("new_epoch_timestamp_ms", &self.new_epoch_timestamp_ms)
            .field("proposal_commit_index", &self.proposal_commit_index)
            .field("proposer", &self.proposer())
            .field("signature", &hex::encode(&self.signature_bytes[..8.min(self.signature_bytes.len())]))
            .field("created_at_seconds", &self.created_at_seconds)
            .finish()
    }
}

impl EpochChangeProposal {
    pub fn new(
        new_epoch: u64,
        new_committee: Committee,
        new_epoch_timestamp_ms: u64,
        proposal_commit_index: u32,
        proposer: AuthorityIndex,
        signature: ProtocolKeySignature,
    ) -> Self {
        let created_at_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self::new_with_signature(
            new_epoch,
            new_committee,
            new_epoch_timestamp_ms,
            proposal_commit_index,
            proposer,
            signature,
            created_at_seconds,
        )
    }
}

/// Vote on an epoch change proposal
/// NOTE: Using derive Serialize/Deserialize for BCS compatibility
#[derive(Serialize, Deserialize)]
pub struct EpochChangeVote {
    pub proposal_hash: Vec<u8>,
    pub voter_value: u32,  // Store as u32 for BCS compatibility
    pub approve: bool,
    pub signature_bytes: Vec<u8>,  // Store as bytes for BCS compatibility
}

impl EpochChangeVote {
    /// Create new vote with AuthorityIndex and ProtocolKeySignature
    pub fn new_with_signature(
        proposal_hash: Vec<u8>,
        voter: AuthorityIndex,
        approve: bool,
        signature: ProtocolKeySignature,
    ) -> Self {
        Self {
            proposal_hash,
            voter_value: voter.value() as u32,
            approve,
            signature_bytes: signature.to_bytes().to_vec(),
        }
    }
    
    /// Get voter as AuthorityIndex
    pub fn voter(&self) -> AuthorityIndex {
        AuthorityIndex::new_for_test(self.voter_value)
    }
    
    /// Get signature as ProtocolKeySignature
    pub fn signature(&self) -> Result<ProtocolKeySignature> {
        ProtocolKeySignature::from_bytes(&self.signature_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize signature: {}", e))
    }
}

impl Clone for EpochChangeVote {
    fn clone(&self) -> Self {
        Self {
            proposal_hash: self.proposal_hash.clone(),
            voter_value: self.voter_value,
            approve: self.approve,
            signature_bytes: self.signature_bytes.clone(),
        }
    }
}

impl fmt::Debug for EpochChangeVote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EpochChangeVote")
            .field("proposal_hash", &hex::encode(&self.proposal_hash[..8.min(self.proposal_hash.len())]))
            .field("voter", &self.voter())
            .field("approve", &self.approve)
            .field("signature", &hex::encode(&self.signature_bytes[..8.min(self.signature_bytes.len())]))
            .finish()
    }
}

/// Manager for epoch change proposals and votes
pub struct EpochChangeManager {
    /// Current epoch
    current_epoch: u64,
    /// Current committee
    committee: Arc<Committee>,
    /// Own authority index
    #[allow(dead_code)] // Used internally for voting
    own_index: AuthorityIndex,
    /// Epoch start timestamp
    epoch_start_timestamp_ms: u64,
    /// Pending proposals (keyed by proposal hash)
    pending_proposals: HashMap<Vec<u8>, EpochChangeProposal>,
    /// Votes for each proposal (keyed by proposal hash, then by voter)
    /// This MUST be de-duplicated by voter to avoid counting stake multiple times.
    proposal_votes: HashMap<Vec<u8>, HashMap<u32, EpochChangeVote>>,
    /// Proposal history (for rate limiting)
    proposal_history: Vec<EpochChangeProposal>,
    /// Seen proposals (for replay attack prevention)
    seen_proposals: HashSet<Vec<u8>>,
    /// Quorum-reached already logged (per proposal hash) to avoid log spam
    quorum_logged: HashSet<Vec<u8>>,
    /// Time-based epoch change configuration
    time_based_enabled: bool,
    epoch_duration_seconds: u64,
    /// Max clock drift allowed (in milliseconds)
    #[allow(dead_code)] // Will be used when implementing clock sync validation
    max_clock_drift_ms: u64,

    /// Throttle noisy time-based check logs.
    last_time_based_check_log_ms: u64,
}

impl EpochChangeManager {
    pub fn new(
        current_epoch: u64,
        committee: Arc<Committee>,
        own_index: AuthorityIndex,
        epoch_start_timestamp_ms: u64,
        time_based_enabled: bool,
        epoch_duration_seconds: u64,
        max_clock_drift_ms: u64,
    ) -> Self {
        Self {
            current_epoch,
            committee,
            own_index,
            epoch_start_timestamp_ms,
            pending_proposals: HashMap::new(),
            proposal_votes: HashMap::new(),
            proposal_history: Vec::new(),
            seen_proposals: HashSet::new(),
            quorum_logged: HashSet::new(),
            time_based_enabled,
            epoch_duration_seconds,
            max_clock_drift_ms,
            last_time_based_check_log_ms: 0,
        }
    }

    /// Reset internal state for a new epoch.
    /// This is used when restarting consensus in-process to ensure epoch is independent
    /// (no old proposals/votes/seen hashes are carried over).
    pub fn reset_for_new_epoch(
        &mut self,
        new_epoch: u64,
        new_committee: Arc<Committee>,
        new_epoch_start_timestamp_ms: u64,
    ) {
        self.current_epoch = new_epoch;
        self.committee = new_committee;
        self.epoch_start_timestamp_ms = new_epoch_start_timestamp_ms;

        // Clear all epoch-scoped state.
        self.pending_proposals.clear();
        self.proposal_votes.clear();
        self.proposal_history.clear();
        self.seen_proposals.clear();
        self.quorum_logged.clear();

        // Reset log throttling state for new epoch.
        self.last_time_based_check_log_ms = 0;
    }

    /// Returns true if we've already logged quorum for this proposal hash.
    pub fn quorum_already_logged(&self, proposal_hash: &[u8]) -> bool {
        self.quorum_logged.contains(proposal_hash)
    }

    /// Mark quorum as logged for this proposal hash.
    pub fn mark_quorum_logged(&mut self, proposal_hash: Vec<u8>) {
        self.quorum_logged.insert(proposal_hash);
    }

    /// Hash a proposal for identification
    pub fn hash_proposal(&self, proposal: &EpochChangeProposal) -> Vec<u8> {
        // Create a serializable version without the skip field
        let proposal_data = format!(
            "{}-{}-{}-{}-{}",
            proposal.new_epoch,
            proposal.new_committee.epoch(),
            proposal.new_epoch_timestamp_ms,
            proposal.proposal_commit_index,
            proposal.proposer().value()
        );
        Blake2b256::digest(proposal_data.as_bytes()).to_vec()
    }

    /// Check if time-based proposal should be triggered
    pub fn should_propose_time_based(&mut self) -> bool {
        if !self.time_based_enabled {
            return false;
        }
        
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        let elapsed_seconds = (now_ms - self.epoch_start_timestamp_ms) / 1000;
        
        // Propose when enough time has elapsed (may drift a few seconds due to clock drift)
        let should_propose = elapsed_seconds >= self.epoch_duration_seconds;
        
        // Log throttling:
        // - When close to threshold (last 30s): log at most once per 5s.
        // - When past threshold: log at most once per 60s to avoid spam while waiting for barrier.
        let remaining = self.epoch_duration_seconds.saturating_sub(elapsed_seconds);
        let close_to_threshold = elapsed_seconds >= self.epoch_duration_seconds.saturating_sub(30);
        let last_ms = self.last_time_based_check_log_ms;
        let min_interval_ms = if should_propose { 60_000 } else { 5_000 };
        if close_to_threshold && now_ms.saturating_sub(last_ms) >= min_interval_ms {
            if should_propose {
                info!(
                    "⏰ EPOCH CHANGE CHECK: epoch={}, elapsed={}s, duration={}s, should_propose=YES (time reached!)",
                    self.current_epoch,
                    elapsed_seconds,
                    self.epoch_duration_seconds
                );
            } else {
                info!(
                    "⏰ EPOCH CHANGE CHECK: epoch={}, elapsed={}s, remaining={}s, duration={}s, should_propose=NO",
                    self.current_epoch,
                    elapsed_seconds,
                    remaining,
                    self.epoch_duration_seconds
                );
            }
            self.last_time_based_check_log_ms = now_ms;
        }
        
        should_propose
    }

    /// Validate clock synchronization
    #[allow(dead_code)] // Will be used when implementing clock sync validation
    pub fn check_clock_sync(&self) -> Result<()> {
        // This will be enhanced when clock sync module is implemented
        // For now, just a placeholder
        Ok(())
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> u64 {
        self.current_epoch
    }

    /// Get committee
    pub fn committee(&self) -> &Committee {
        &self.committee
    }

    /// Propose epoch change
    pub fn propose_epoch_change(
        &mut self,
        new_committee: Committee,
        new_epoch_timestamp_ms: u64,
        proposal_commit_index: u32,
        proposer: AuthorityIndex,
        proposer_keypair: &ProtocolKeyPair,
    ) -> Result<EpochChangeProposal> {
        // Check rate limit
        self.check_rate_limit(proposer)?;

        // Validate new committee
        self.validate_new_committee(&new_committee)?;

        let new_epoch = self.current_epoch + 1;

        // Create proposal message to sign
        let proposal_message = format!(
            "epoch_change:{}:{}:{}:{}:{}",
            new_epoch,
            new_committee.epoch(),
            new_epoch_timestamp_ms,
            proposal_commit_index,
            proposer.value()
        );

        // Sign proposal
        let signature = proposer_keypair.sign(proposal_message.as_bytes());

        let proposal = EpochChangeProposal::new(
            new_epoch,
            new_committee,
            new_epoch_timestamp_ms,
            proposal_commit_index,
            proposer,
            signature,
        );

        // Check for duplicate
        let proposal_hash = self.hash_proposal(&proposal);
        if self.seen_proposals.contains(&proposal_hash) {
            return Err(EpochChangeError::DuplicateProposal.into());
        }

        // Store proposal
        self.pending_proposals.insert(proposal_hash.clone(), proposal.clone());
        self.seen_proposals.insert(proposal_hash.clone());
        self.proposal_history.push(proposal.clone());

        let hash_hex = hex::encode(&proposal_hash[..8]);
        info!(
            "📝 Epoch change proposal created: epoch {} -> {}, proposal_hash={}, proposer={}, commit_index={}, timestamp={}",
            self.current_epoch,
            new_epoch,
            hash_hex,
            proposer.value(),
            proposal_commit_index,
            new_epoch_timestamp_ms
        );

        Ok(proposal)
    }

    /// Get epoch start timestamp
    pub fn epoch_start_timestamp_ms(&self) -> u64 {
        self.epoch_start_timestamp_ms
    }

    /// Get epoch duration in seconds
    pub fn epoch_duration_seconds(&self) -> u64 {
        self.epoch_duration_seconds
    }

    /// Get pending proposals count
    #[allow(dead_code)] // Reserved for future use (monitoring, debugging)
    pub fn pending_proposals_count(&self) -> usize {
        self.pending_proposals.len()
    }

    /// Get votes count for a proposal
    pub fn get_proposal_votes_count(&self, proposal: &EpochChangeProposal) -> usize {
        let proposal_hash = self.hash_proposal(proposal);
        self.proposal_votes
            .get(&proposal_hash)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Vote on a proposal
    #[allow(dead_code)] // Will be used when implementing auto-vote mechanism
    pub fn vote_on_proposal(
        &mut self,
        proposal: &EpochChangeProposal,
        voter: AuthorityIndex,
        voter_keypair: &ProtocolKeyPair,
    ) -> Result<EpochChangeVote> {
        // Validate proposal
        self.validate_proposal(proposal)?;

        // Decide vote (approve if valid)
        let approve = self.should_approve_proposal(proposal)?;

        // Create vote message
        let proposal_hash = self.hash_proposal(proposal);
        let vote_message = format!(
            "epoch_vote:{}:{}:{}",
            hex::encode(&proposal_hash[..8]),
            approve,
            self.committee.authority(proposal.proposer()).stake
        );

        // Sign vote
        let signature = voter_keypair.sign(vote_message.as_bytes());

        let vote = EpochChangeVote::new_with_signature(
            proposal_hash.clone(),
            self.get_own_index(),
            approve,
            signature,
        );

        // Store vote
        let proposal_hash_clone = proposal_hash.clone();
        // Store vote de-duplicated by voter.
        self.proposal_votes
            .entry(proposal_hash_clone)
            .or_insert_with(HashMap::new)
            .insert(voter.value() as u32, vote.clone());

        let proposal_hash_hex = hex::encode(&proposal_hash[..8.min(proposal_hash.len())]);
        info!(
            "🗳️  Voted on epoch change proposal: proposal_hash={}, epoch {} -> {}, voter={}, approve={}",
            proposal_hash_hex,
            proposal.new_epoch - 1,
            proposal.new_epoch,
            voter.value(),
            approve
        );

        Ok(vote)
    }

    /// Check if proposal has quorum
    pub fn check_proposal_quorum(
        &self,
        proposal: &EpochChangeProposal,
    ) -> Option<bool> {
        let proposal_hash = self.hash_proposal(proposal);
        let votes_map = self.proposal_votes.get(&proposal_hash)?;
        let votes: Vec<&EpochChangeVote> = votes_map.values().collect();

        let approve_stake: Stake = votes
            .iter()
            .filter(|v| v.approve)
            .map(|v| {
                let voter = v.voter();
                if self.committee.is_valid_index(voter) {
                    self.committee.authority(voter).stake
                } else {
                    0
                }
            })
            .sum();

        let reject_stake: Stake = votes
            .iter()
            .filter(|v| !v.approve)
            .map(|v| {
                let voter = v.voter();
                if self.committee.is_valid_index(voter) {
                    self.committee.authority(voter).stake
                } else {
                    0
                }
            })
            .sum();

        let quorum_threshold = self.committee.quorum_threshold();
        let total_stake = self.committee.total_stake();
        
        // Check quorum approve
        if approve_stake >= quorum_threshold {
            // Do NOT log here (this function is called in many loops and would spam).
            return Some(true);
        }

        // Check quorum reject
        if reject_stake >= quorum_threshold {
            // Do NOT log here (avoid spam).
            return Some(false);
        }

        // Log quorum progress periodically (every 5 votes or when significant change)
        if votes.len() % 5 == 0 || approve_stake + reject_stake >= quorum_threshold / 2 {
            let proposal_hash_hex = hex::encode(&proposal_hash[..8]);
            info!(
                "📊 Quorum progress: proposal_hash={}, epoch {} -> {}, approve_stake={}/{}, reject_stake={}/{}, threshold={}, votes={}",
                proposal_hash_hex,
                proposal.new_epoch - 1,
                proposal.new_epoch,
                approve_stake,
                total_stake,
                reject_stake,
                total_stake,
                quorum_threshold,
                votes.len()
            );
        }

        None // Not reached quorum yet
    }

    /// Validate proposal
    #[allow(dead_code)] // Used internally by process_proposal
    pub fn validate_proposal(&self, proposal: &EpochChangeProposal) -> Result<()> {
        // 1. Validate epoch increment
        ensure!(
            proposal.new_epoch == self.current_epoch + 1,
            "Epoch must increment by 1"
        );

        // 2. Validate committee
        self.validate_new_committee(&proposal.new_committee)?;

        // 3. Validate timestamp (allow recent past to account for network delay)
        // For time-based epoch change, timestamp can be in the past (within 5 minutes)
        // to account for network propagation and processing delays
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis() as u64;
        let timestamp_diff_ms = now_ms as i64 - proposal.new_epoch_timestamp_ms as i64;
        const MAX_PAST_MS: i64 = 5 * 60 * 1000; // 5 minutes in milliseconds
        const MAX_FUTURE_MS: i64 = 24 * 60 * 60 * 1000; // 24 hours in milliseconds
        
        ensure!(
            timestamp_diff_ms <= MAX_PAST_MS && timestamp_diff_ms >= -MAX_FUTURE_MS,
            "Epoch timestamp must be within reasonable range (past: {}ms, future: {}ms, diff: {}ms)",
            MAX_PAST_MS,
            MAX_FUTURE_MS,
            timestamp_diff_ms
        );

        // 4. Validate proposer
        ensure!(
            self.committee.is_valid_index(proposal.proposer()),
            "Invalid proposer authority index"
        );

        // 5. Validate signature
        self.verify_proposal_signature(proposal)?;

        Ok(())
    }

    /// Validate new committee
    fn validate_new_committee(&self, new: &Committee) -> Result<()> {
        ensure!(new.size() >= 4, "Minimum 4 nodes required");
        
        // Validate quorum threshold
        let f = (new.total_stake() - 1) / 3;
        ensure!(
            new.quorum_threshold() == new.total_stake() - f,
            "Invalid quorum threshold"
        );

        // Validate epoch increment
        ensure!(
            new.epoch() == self.current_epoch + 1,
            "Epoch must increment by 1"
        );

        Ok(())
    }

    /// Verify proposal signature
    #[allow(dead_code)] // Used internally by validate_proposal
    fn verify_proposal_signature(&self, proposal: &EpochChangeProposal) -> Result<()> {
        let proposal_message = format!(
            "epoch_change:{}:{}:{}:{}:{}",
            proposal.new_epoch,
            proposal.new_committee.epoch(),
            proposal.new_epoch_timestamp_ms,
            proposal.proposal_commit_index,
            proposal.proposer().value()
        );

        // Get proposer's public key
        let proposer_auth = self.committee.authority(proposal.proposer());
        let public_key = &proposer_auth.protocol_key;

        // Verify signature
        let signature = proposal.signature()?;
        public_key.verify(proposal_message.as_bytes(), &signature)
            .map_err(|e| anyhow::anyhow!("Invalid proposal signature: {:?}", e))?;

        Ok(())
    }

    /// Check for replay attack
    /// NOTE: Only reject if proposal is already pending (not just seen)
    /// This allows proposals to be included in multiple blocks for propagation
    #[allow(dead_code)] // Used internally by validate_proposal
    fn check_replay_attack(&self, proposal: &EpochChangeProposal) -> Result<()> {
        let proposal_hash = self.hash_proposal(proposal);
        // Only reject if proposal is already pending (active replay attack)
        // If proposal was seen but not pending, it might have expired/been removed
        if self.pending_proposals.contains_key(&proposal_hash) {
            anyhow::bail!("Replay attack detected: proposal already processed");
        }
        Ok(())
    }

    /// Should approve proposal (basic logic - can be enhanced)
    #[allow(dead_code)] // Used internally by vote_on_proposal
    fn should_approve_proposal(&self, proposal: &EpochChangeProposal) -> Result<bool> {
        // For now, approve if committee is valid
        // Can add more complex logic later
        self.validate_new_committee(&proposal.new_committee)?;
        Ok(true)
    }

    /// Get own index
    #[allow(dead_code)] // Used internally by vote_on_proposal
    fn get_own_index(&self) -> AuthorityIndex {
        self.own_index
    }

    /// Check rate limit
    fn check_rate_limit(&self, proposer: AuthorityIndex) -> Result<()> {
        const MAX_PROPOSALS_PER_HOUR: u32 = 10;
        const MAX_PROPOSALS_PER_EPOCH: u32 = 3;

        let now = SystemTime::now();
        let hour_ago = now - Duration::from_secs(3600);

        let recent_proposals = self.proposal_history
            .iter()
            .filter(|p| {
                p.proposer() == proposer
                    && SystemTime::UNIX_EPOCH + Duration::from_secs(p.created_at_seconds) > hour_ago
            })
            .count();

        if recent_proposals >= MAX_PROPOSALS_PER_HOUR as usize {
            anyhow::bail!("Rate limit exceeded: {} proposals in last hour", recent_proposals);
        }

        let epoch_proposals = self.proposal_history
            .iter()
            .filter(|p| p.proposer() == proposer && p.new_epoch == self.current_epoch + 1)
            .count();

        if epoch_proposals >= MAX_PROPOSALS_PER_EPOCH as usize {
            anyhow::bail!("Rate limit exceeded: {} proposals for next epoch", epoch_proposals);
        }

        Ok(())
    }

    /// Get pending proposal to broadcast
    #[allow(dead_code)] // Will be used when implementing block integration
    pub fn get_pending_proposal_to_broadcast(&self) -> Option<EpochChangeProposal> {
        // Return first pending proposal
        self.pending_proposals.values().next().cloned()
    }

    /// Get pending votes to broadcast
    #[allow(dead_code)] // Will be used when implementing block integration
    pub fn get_pending_votes_to_broadcast(&self) -> Vec<EpochChangeVote> {
        // Broadcast at most 1 vote per (proposal, voter).
        // Also, if a proposal already reached a terminal quorum, we stop broadcasting its votes.
        let mut out = Vec::new();
        for proposal in self.pending_proposals.values() {
            if let Some(decision) = self.check_proposal_quorum(proposal) {
                // Terminal state reached, no need to spam network with votes.
                let _ = decision;
                continue;
            }
            let proposal_hash = self.hash_proposal(proposal);
            if let Some(votes_by_voter) = self.proposal_votes.get(&proposal_hash) {
                out.extend(votes_by_voter.values().cloned());
            }
        }
        out
    }

    /// Get a previously stored vote for (proposal_hash, voter), if any.
    pub fn get_vote_by_voter(&self, proposal_hash: &[u8], voter_value: u32) -> Option<EpochChangeVote> {
        self.proposal_votes
            .get(proposal_hash)
            .and_then(|m| m.get(&voter_value))
            .cloned()
    }

    /// Returns true if we currently track this proposal as pending (active) in this epoch.
    pub fn has_pending_proposal_hash(&self, proposal_hash: &[u8]) -> bool {
        self.pending_proposals.contains_key(proposal_hash)
    }

    /// Process a proposal received from another node
    #[allow(dead_code)] // Will be used when implementing block processing
    pub fn process_proposal(&mut self, proposal: EpochChangeProposal) -> Result<()> {
        let proposal_hash = self.hash_proposal(&proposal);
        
        // Idempotent: If proposal already processed and pending, skip
        if self.pending_proposals.contains_key(&proposal_hash) {
            // Proposal already processed and pending - this is normal when proposal
            // is included in multiple blocks for propagation
            return Ok(());
        }
        
        // Validate proposal only if not already processed
        self.validate_proposal(&proposal)?;

        // Store proposal
        self.pending_proposals.insert(proposal_hash.clone(), proposal.clone());
        self.seen_proposals.insert(proposal_hash);
        self.proposal_history.push(proposal);

        Ok(())
    }

    /// Process a vote received from another node
    #[allow(dead_code)] // Will be used when implementing block processing
    pub fn process_vote(&mut self, vote: EpochChangeVote) -> Result<()> {
        // Validate vote
        self.validate_vote(&vote)?;

        // Store vote de-duplicated by voter.
        self.proposal_votes
            .entry(vote.proposal_hash.clone())
            .or_insert_with(HashMap::new)
            .insert(vote.voter_value, vote);

        Ok(())
    }

    /// Validate vote
    #[allow(dead_code)] // Used internally by process_vote
    pub fn validate_vote(&self, vote: &EpochChangeVote) -> Result<()> {
        // Check if proposal exists
        if !self.pending_proposals.contains_key(&vote.proposal_hash) {
            anyhow::bail!("Vote for unknown proposal");
        }

        // Validate voter
        let voter = vote.voter();
        ensure!(
            self.committee.is_valid_index(voter),
            "Invalid voter authority index"
        );

        // Verify signature
        let voter_auth = self.committee.authority(voter);
        let public_key = &voter_auth.protocol_key;

        let vote_message = format!(
            "epoch_vote:{}:{}:{}",
            hex::encode(&vote.proposal_hash[..8]),
            vote.approve,
            voter_auth.stake
        );

        let signature = vote.signature()?;
        public_key.verify(vote_message.as_bytes(), &signature)
            .map_err(|e| anyhow::anyhow!("Invalid vote signature: {:?}", e))?;

        Ok(())
    }

    /// Get approved proposal (if any)
    #[allow(dead_code)] // Reserved for future use (transition monitoring)
    pub fn get_approved_proposal(&self) -> Option<EpochChangeProposal> {
        for proposal in self.pending_proposals.values() {
            if let Some(true) = self.check_proposal_quorum(proposal) {
                return Some(proposal.clone());
            }
        }
        None
    }

    /// Check if should transition to new epoch (fork-safe)
    /// This ensures all nodes transition at the same commit index
    /// 
    /// Fork-safety mechanism:
    /// 1. Quorum must be reached (2f+1 votes)
    /// 2. All nodes must reach the same commit index barrier
    /// 3. Buffer of 10 commits ensures all nodes have processed the proposal
    pub fn should_transition(
        &self,
        proposal: &EpochChangeProposal,
        current_commit_index: u32,
    ) -> bool {
        // ✅ Điều kiện 1: Quorum đã đạt
        let quorum_reached = self.check_proposal_quorum(proposal)
            .map(|approved| approved)
            .unwrap_or(false);
        
        if !quorum_reached {
            return false;
        }
        
        // ✅ Điều kiện 2: Đã đến commit index được chỉ định
        // Tất cả nodes sẽ transition tại cùng commit_index → fork-safe
        // 
        // IMPORTANT: Buffer of 10 commits ensures:
        // - Proposal has been committed and propagated
        // - All nodes have had time to reach this commit index
        // - Reduces risk of fork due to network delay
        let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
        
        // Use >= to allow transition once barrier is reached
        // All nodes will transition when they reach this index
        // Small differences in commit index between nodes are acceptable
        // as long as they're all >= transition_commit_index
        let ready = current_commit_index >= transition_commit_index;
        
        if ready {
            // Log for monitoring - ensure all nodes transition at similar commit index
            tracing::debug!(
                "✅ Transition ready: current_commit={}, barrier={}, diff={}",
                current_commit_index,
                transition_commit_index,
                current_commit_index.saturating_sub(transition_commit_index)
            );
        }
        
        ready
    }

    /// Get proposal ready for transition (quorum reached + commit index barrier passed)
    pub fn get_transition_ready_proposal(
        &self,
        current_commit_index: u32,
    ) -> Option<EpochChangeProposal> {
        for proposal in self.pending_proposals.values() {
            if self.should_transition(proposal, current_commit_index) {
                return Some(proposal.clone());
            }
        }
        None
    }

    /// Check if there's a pending proposal for a specific epoch
    pub fn has_pending_proposal_for_epoch(&self, epoch: u64) -> bool {
        self.pending_proposals.values()
            .any(|p| p.new_epoch == epoch)
    }

    /// Get all pending proposals (for logging)
    pub fn get_all_pending_proposals(&self) -> Vec<EpochChangeProposal> {
        self.pending_proposals.values().cloned().collect()
    }

    /// Check proposal timeout
    #[allow(dead_code)] // Will be used when implementing proposal cleanup
    pub fn check_proposal_timeout(&self, proposal: &EpochChangeProposal) -> bool {
        const PROPOSAL_TIMEOUT_SECONDS: u64 = 300; // 5 minutes

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let proposal_age = now.saturating_sub(proposal.created_at_seconds);
        proposal_age > PROPOSAL_TIMEOUT_SECONDS
    }

    /// Cleanup expired proposals
    #[allow(dead_code)] // Will be used when implementing periodic cleanup
    pub fn cleanup_expired_proposals(&mut self) {
        let expired: Vec<_> = self.pending_proposals
            .iter()
            .filter(|(_, p)| self.check_proposal_timeout(p))
            .map(|(hash, _)| hash.clone())
            .collect();

        for hash in expired {
            self.pending_proposals.remove(&hash);
            info!("Removed expired proposal: {}", hex::encode(&hash[..8]));
        }
    }
}

// Helper functions for serialization/deserialization
impl EpochChangeProposal {
    /// Serialize proposal to bytes
    #[allow(dead_code)] // Will be used when implementing block integration
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        bcs::to_bytes(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize proposal: {}", e))
    }

    /// Deserialize proposal from bytes
    #[allow(dead_code)] // Will be used when implementing block processing
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        bcs::from_bytes(bytes)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize proposal: {}", e))
    }
}

impl EpochChangeVote {
    /// Serialize vote to bytes
    #[allow(dead_code)] // Will be used when implementing block integration
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        bcs::to_bytes(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize vote: {}", e))
    }

    /// Deserialize vote from bytes
    #[allow(dead_code)] // Will be used when implementing block processing
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        bcs::from_bytes(bytes)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize vote: {}", e))
    }
}

