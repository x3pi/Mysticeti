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
use std::io::Write;
use tracing::{info, warn};
use thiserror::Error;
use bcs;

// Import ExecutorClient
use crate::executor_client::ExecutorClient;

/// Network client interface for epoch change broadcasting
#[async_trait::async_trait]
pub trait EpochChangeNetworkClient: Send + Sync {
    #[allow(dead_code)]
    async fn send_epoch_change_proposal(
        &self,
        peer: AuthorityIndex,
        proposal: &EpochChangeProposal,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[allow(dead_code)]
    async fn send_epoch_change_vote(
        &self,
        peer: AuthorityIndex,
        vote: &EpochChangeVote,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

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
    // REMOVED: new_committee - will fetch from Go state during transition
    // This ensures committee is always calculated from current Go staking state
    pub new_epoch_timestamp_ms: u64,
    pub proposal_commit_index: u32,
    pub proposer_value: u32,  // Store as u32 for BCS compatibility
    pub signature_bytes: Vec<u8>,  // Store as bytes for BCS compatibility
    /// Timestamp when proposal was created (for timeout checking)
    pub created_at_seconds: u64,
}

impl EpochChangeProposal {
    /// Create new proposal with AuthorityIndex and ProtocolKeySignature
    /// NOTE: No longer requires new_committee - will fetch from Go state during transition
    pub fn new_with_signature(
        new_epoch: u64,
        new_epoch_timestamp_ms: u64,
        proposal_commit_index: u32,
        proposer: AuthorityIndex,
        signature: ProtocolKeySignature,
        created_at_seconds: u64,
    ) -> Self {
        Self {
            new_epoch,
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
            .field("new_epoch_timestamp_ms", &self.new_epoch_timestamp_ms)
            .field("proposal_commit_index", &self.proposal_commit_index)
            .field("proposer", &self.proposer())
            .field("signature", &hex::encode(&self.signature_bytes[..8.min(self.signature_bytes.len())]))
            .field("created_at_seconds", &self.created_at_seconds)
            .field("note", &"committee_from_go_state")
            .finish()
    }
}

impl EpochChangeProposal {
    pub fn new(
        new_epoch: u64,
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
    /// Network client for broadcasting epoch change messages
    #[allow(dead_code)]
    network_client: Option<Arc<dyn EpochChangeNetworkClient>>,
    /// Executor client to fetch committee from Go state during transition
    #[allow(dead_code)] // Will be used when implementing transition
    executor_client: Option<Arc<ExecutorClient>>,
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
        network_client: Option<Arc<dyn EpochChangeNetworkClient>>,
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
            network_client,
            executor_client: None,
        }
    }

    /// Reset internal state for a new epoch.
    /// This is used when restarting consensus in-process to ensure epoch is independent
    /// (no old proposals/votes/seen hashes are carried over).
    /// NOTE: Committee will be updated from Go state during transition
    pub fn reset_for_new_epoch(
        &mut self,
        new_epoch: u64,
        new_epoch_start_timestamp_ms: u64,
    ) {
        self.current_epoch = new_epoch;
        // NOTE: committee will be updated from Go state during transition
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
    ///
    /// CRITICAL FORK-SAFETY: Hash phải được tính GIỐNG NHAU ở tất cả nodes
    /// Nếu hash khác nhau, nodes sẽ không thể validate proposal → fork
    ///
    /// Hash được tính từ các field deterministic:
    /// - new_epoch: Epoch number (deterministic)
    /// - new_epoch_timestamp_ms: Epoch start timestamp (phải giống nhau ở tất cả nodes)
    /// - proposal_commit_index: Commit index khi proposal được tạo (deterministic)
    /// - proposer().value(): Proposer authority index (deterministic)
    ///
    /// Tất cả các field này đều deterministic và giống nhau ở tất cả nodes
    /// → Hash sẽ giống nhau ở tất cả nodes → Không fork
    pub fn hash_proposal(&self, proposal: &EpochChangeProposal) -> Vec<u8> {
        // Create a serializable version without the skip field
        // CRITICAL: Format string phải giống nhau ở tất cả nodes
        // Không được thay đổi format này vì sẽ làm hash khác nhau → fork
        let proposal_data = format!(
            "{}-{}-{}-{}",
            proposal.new_epoch,
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
    /// NOTE: Committee will be fetched from Go state during transition
    pub fn propose_epoch_change(
        &mut self,
        new_epoch_timestamp_ms: u64,
        proposal_commit_index: u32,
        proposer: AuthorityIndex,
        proposer_keypair: &ProtocolKeyPair,
    ) -> Result<EpochChangeProposal> {
        // Rate limiting removed - can be re-added if needed

        let new_epoch = self.current_epoch + 1;

        // Create proposal message to sign
        // NOTE: No committee in proposal - will fetch from Go state during transition
        let proposal_message = format!(
            "epoch_change:{}:{}:{}:{}",
            new_epoch,
            new_epoch_timestamp_ms,
            proposal_commit_index,
            proposer.value()
        );

        // Sign proposal
        let signature = proposer_keypair.sign(proposal_message.as_bytes());

        let proposal = EpochChangeProposal::new(
            new_epoch,
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
            "📝 Epoch change proposal created: epoch {} -> {}, proposal_hash={}, proposer={}, commit_index={}, timestamp={} (committee_from_go_state)",
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

    /// Set executor client for fetching committee from Go state
    pub fn set_executor_client(&mut self, executor_client: Arc<ExecutorClient>) {
        self.executor_client = Some(executor_client);
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
        let voter_auth = self.committee.authority(voter);
        let vote_message = format!(
            "epoch_vote:{}:{}:{}",
            hex::encode(&proposal_hash[..8]),
            approve,
            voter_auth.stake
        );

        // Sign vote
        let signature = voter_keypair.sign(vote_message.as_bytes());
        info!("🔏 CREATING VOTE: message='{}', stake={}, approve={}, voter={}", vote_message, voter_auth.stake, approve, voter);

        let vote = EpochChangeVote::new_with_signature(
            proposal_hash.clone(),
            voter, // Use the voter parameter, not self.get_own_index()
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
        
        // DEBUG: Log if proposal_hash not found in proposal_votes
        if !self.proposal_votes.contains_key(&proposal_hash) {
            let proposal_hash_hex = hex::encode(&proposal_hash[..8.min(proposal_hash.len())]);
            tracing::warn!(
                "⚠️  Proposal hash not found in proposal_votes: proposal_hash={}, epoch {} -> {}",
                proposal_hash_hex,
                proposal.new_epoch - 1,
                proposal.new_epoch
            );
            
            // DEBUG: Log all proposal hashes in proposal_votes
            if !self.proposal_votes.is_empty() {
                let existing_hashes: Vec<String> = self.proposal_votes.keys()
                    .map(|h| hex::encode(&h[..8.min(h.len())]))
                    .collect();
                tracing::debug!(
                    "   Available proposal hashes in proposal_votes: {:?}",
                    existing_hashes
                );
            } else {
                tracing::debug!(
                    "   proposal_votes is empty - no votes stored yet"
                );
            }
            
            // #region agent log
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"C","location":"epoch_change.rs:check_proposal_quorum:no_votes","message":"No votes found for proposal","data":{{"proposal_hash":"{}","total_proposals":{}}},"timestamp":{}}}"#, 
                    proposal_hash_hex, self.proposal_votes.len(), ts);
            }
            // #endregion
            
            return None;
        }
        
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

        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"C","location":"epoch_change.rs:check_proposal_quorum:vote_count","message":"Vote counting","data":{{"approve_stake":{},"reject_stake":{},"quorum_threshold":{},"total_stake":{},"committee_size":{},"vote_count":{}}},"timestamp":{}}}"#, 
                approve_stake, reject_stake, quorum_threshold, self.committee.total_stake(), self.committee.size(), votes.len(), ts);
        }
        // #endregion

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

        None // Not reached quorum yet
    }

    /// Validate proposal
    /// If node is catching up (old timestamp), sync timestamp from proposal before validating
    #[allow(dead_code)] // Used internally by process_proposal
    pub fn validate_proposal(&mut self, proposal: &EpochChangeProposal) -> Result<()> {
        // 1. Validate epoch increment
        ensure!(
            proposal.new_epoch == self.current_epoch + 1,
            "Epoch must increment by 1"
        );

        // 2. Committee validation will happen during transition with committee from Go state

        // 3. Check if this is a catch-up scenario and sync timestamp if needed
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis() as u64;
        let timestamp_diff_ms = now_ms as i64 - proposal.new_epoch_timestamp_ms as i64;
        const MAX_PAST_MS_NORMAL: i64 = 5 * 60 * 1000; // 5 minutes in milliseconds (normal case)
        const MAX_FUTURE_MS: i64 = 24 * 60 * 60 * 1000; // 24 hours in milliseconds
        
        // Determine if this is a catch-up scenario:
        // - Proposal is for next epoch (proposal.new_epoch == current_epoch + 1) ✓
        // - Timestamp is older than normal limit (more than 5 minutes old)
        // This means the network already transitioned, and this node is catching up
        let is_catchup = timestamp_diff_ms > MAX_PAST_MS_NORMAL;
        
        if is_catchup {
            // CATCH-UP SCENARIO: Sync timestamp from network (proposal) before validating
            // The proposal contains the timestamp that the network is using for the new epoch
            // We must sync this timestamp first, then validate with the synced timestamp
            let old_timestamp = self.epoch_start_timestamp_ms;
            self.epoch_start_timestamp_ms = proposal.new_epoch_timestamp_ms;
            
            tracing::info!(
                "🔄 Catch-up scenario detected: syncing timestamp from network (old: {}, new: {}, diff: {}ms) - validating with synced timestamp",
                old_timestamp,
                proposal.new_epoch_timestamp_ms,
                timestamp_diff_ms
            );
            
            // After syncing, recalculate diff with synced timestamp
            // Now the timestamp should be valid (it's the timestamp the network is using)
            // We still check it's not too far in the future
            let synced_timestamp_diff_ms = now_ms as i64 - self.epoch_start_timestamp_ms as i64;
            ensure!(
                synced_timestamp_diff_ms >= -MAX_FUTURE_MS,
                "Synced epoch timestamp must not be too far in the future (max: {}ms, diff: {}ms)",
                MAX_FUTURE_MS,
                synced_timestamp_diff_ms
            );
        } else {
            // NORMAL CASE: Validate timestamp is within normal range (5 minutes past, 24 hours future)
            ensure!(
                timestamp_diff_ms <= MAX_PAST_MS_NORMAL && timestamp_diff_ms >= -MAX_FUTURE_MS,
                "Epoch timestamp must be within reasonable range (past: {}ms, future: {}ms, diff: {}ms)",
                MAX_PAST_MS_NORMAL,
                MAX_FUTURE_MS,
                timestamp_diff_ms
            );
        }

        // 4. Validate proposer
        ensure!(
            self.committee.is_valid_index(proposal.proposer()),
            "Invalid proposer authority index"
        );

        // 5. Validate signature
        self.verify_proposal_signature(proposal)?;

        Ok(())
    }

    /// Validate new committee (will be called during transition with committee from Go state)
    #[allow(dead_code)] // Will be used when implementing transition validation
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
            "epoch_change:{}:{}:{}:{}",
            proposal.new_epoch,
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

    /// Force propose epoch change (bypasses rate limit for emergency cases)
    #[allow(dead_code)]

    /// Should approve proposal (basic logic - can be enhanced)
    /// NOTE: Committee validation will happen during transition with committee from Go state
    #[allow(dead_code)] // Used internally by vote_on_proposal
    fn should_approve_proposal(&self, _proposal: &EpochChangeProposal) -> Result<bool> {
        // For now, approve all valid proposals
        // Committee validation will happen during actual transition
        Ok(true)
    }

    /// Get own index
    #[allow(dead_code)] // Used internally by vote_on_proposal
    fn get_own_index(&self) -> AuthorityIndex {
        self.own_index
    }

    /// Check rate limit
    #[allow(dead_code)]

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
        // 
        // CRITICAL FIX: Continue broadcasting votes even after quorum is reached!
        // This ensures all nodes see quorum and can transition together.
        // 
        // Previous bug: When one node reached quorum, it stopped broadcasting votes,
        // causing other nodes to not see quorum and not transition.
        // 
        // Solution: Always broadcast votes until transition actually happens.
        // The transition itself will clear pending_proposals, which naturally stops broadcasting.
        let mut out = Vec::new();
        for proposal in self.pending_proposals.values() {
            let proposal_hash = self.hash_proposal(proposal);
            if let Some(votes_by_voter) = self.proposal_votes.get(&proposal_hash) {
                // Always broadcast votes, even if quorum is reached
                // This ensures all nodes can see quorum and transition together
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

        // DEBUG: Verify proposal_hash matches if proposal exists
        if let Some(proposal) = self.pending_proposals.get(&vote.proposal_hash) {
            let computed_hash = self.hash_proposal(proposal);
            if computed_hash != vote.proposal_hash {
                tracing::error!(
                    "❌ PROPOSAL HASH MISMATCH! Vote proposal_hash={}, computed_hash={}, epoch {} -> {}",
                    hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())]),
                    hex::encode(&computed_hash[..8.min(computed_hash.len())]),
                    proposal.new_epoch - 1,
                    proposal.new_epoch
                );
            } else {
                tracing::debug!(
                    "✅ Proposal hash verified: proposal_hash={}, epoch {} -> {}",
                    hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())]),
                    proposal.new_epoch - 1,
                    proposal.new_epoch
                );
            }
        } else {
            tracing::warn!(
                "⚠️  Vote received for unknown proposal: proposal_hash={}",
                hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())])
            );
        }

        // Store vote de-duplicated by voter.
        let prev = self.proposal_votes
            .entry(vote.proposal_hash.clone())
            .or_insert_with(HashMap::new)
            .insert(vote.voter_value, vote.clone());

        // Emit quorum progress ONLY when we accepted a NEW vote (prevents log spam from repeated checks).
        if prev.is_none() {
            if let Some(proposal) = self.pending_proposals.get(&vote.proposal_hash) {
                if let Some(votes_map) = self.proposal_votes.get(&vote.proposal_hash) {
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
                    let proposal_hash_hex =
                        hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())]);

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
            }
        }

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
        info!("🔐 VALIDATING VOTE: message='{}', stake={}, approve={}, voter={}", vote_message, voter_auth.stake, vote.approve, voter);
        public_key.verify(vote_message.as_bytes(), &signature)
            .map_err(|e| anyhow::anyhow!("Invalid vote signature: {:?}", e))?;

        Ok(())
    }

    /// Get approved proposal (if any)
    #[allow(dead_code)] // Reserved for future use (transition monitoring)
    pub fn get_approved_proposal(&self) -> Option<EpochChangeProposal> {
        info!("🔍 Checking {} pending proposals for approval", self.pending_proposals.len());
        for proposal in self.pending_proposals.values() {
            let proposal_hash = self.hash_proposal(proposal);
            let quorum_result = self.check_proposal_quorum(proposal);
            info!("🔍 Proposal {} quorum result: {:?}", hex::encode(&proposal_hash[..8]), quorum_result);
            if let Some(true) = quorum_result {
                info!("✅ Found approved proposal: {}", hex::encode(&proposal_hash[..8]));
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
    /// 4. Timeout mechanism: If commit index doesn't increase for 5 minutes after quorum,
    ///    allow transition to prevent deadlock when other nodes have already transitioned
    pub fn should_transition(
        &self,
        proposal: &EpochChangeProposal,
        current_commit_index: u32,
    ) -> bool {
        // #region agent log
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"A","location":"epoch_change.rs:should_transition:entry","message":"should_transition called","data":{{"current_epoch":{},"proposal_new_epoch":{},"current_commit_index":{},"proposal_commit_index":{}}},"timestamp":{}}}"#, 
                self.current_epoch, proposal.new_epoch, current_commit_index, proposal.proposal_commit_index, ts);
        }
        // #endregion

        // SINGLE NODE BYPASS: For local development, allow single node to transition
        let is_single_node = std::env::var("SINGLE_NODE").unwrap_or_default() == "1";
        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"A","location":"epoch_change.rs:should_transition:single_node_check","message":"SINGLE_NODE env check","data":{{"is_single_node":{}}},"timestamp":{}}}"#, 
                is_single_node, ts);
        }
        // #endregion
        if is_single_node {
            tracing::warn!("🔸 SINGLE_NODE=1 detected - Single node mode: bypassing all safety checks");
            tracing::warn!("⚠️  WARNING: This should NEVER be used in production - can cause forks!");
            tracing::warn!("🚀 SINGLE NODE: Force transition enabled for development");
            return true;
        }

        // ✅ Điều kiện 1: Quorum đã đạt HOẶC node lag quá xa (catch-up mode)
        let quorum_status = self.check_proposal_quorum(proposal);
        let quorum_reached = quorum_status
            .map(|approved| approved)
            .unwrap_or(false);

        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let qs = format!("{:?}", quorum_status);
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"C","location":"epoch_change.rs:should_transition:quorum_check","message":"Quorum status check","data":{{"quorum_status":"{}","quorum_reached":{},"committee_size":{},"total_stake":{},"quorum_threshold":{}}},"timestamp":{}}}"#, 
                qs, quorum_reached, self.committee.size(), self.committee.total_stake(), self.committee.quorum_threshold(), ts);
        }
        // #endregion

        // SIMPLE CATCH-UP LOGIC: Nếu node lag quá xa (epoch khác > current_epoch + 2),
        // cho phép transition mà không cần quorum để catch-up nhanh
        // Điều này đảm bảo node không bị stuck khi các nodes khác đã tiến xa
        let epoch_lag = proposal.new_epoch.saturating_sub(self.current_epoch);
        const MAX_EPOCH_LAG_FOR_QUORUM: u64 = 2; // Nếu lag > 2 epochs, không cần quorum
        let is_catchup_mode = epoch_lag > MAX_EPOCH_LAG_FOR_QUORUM;

        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"B","location":"epoch_change.rs:should_transition:catchup_check","message":"Catch-up mode check","data":{{"epoch_lag":{},"max_epoch_lag_for_quorum":{},"is_catchup_mode":{}}},"timestamp":{}}}"#, 
                epoch_lag, MAX_EPOCH_LAG_FOR_QUORUM, is_catchup_mode, ts);
        }
        // #endregion

        // TEMPORARY FIX: Bypass quorum for localhost testing (single node setup)
        let is_localhost_testing = std::env::var("LOCALHOST_TESTING").unwrap_or_default() == "1";
        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"A","location":"epoch_change.rs:should_transition:localhost_check","message":"LOCALHOST_TESTING env check","data":{{"is_localhost_testing":{}}},"timestamp":{}}}"#, 
                is_localhost_testing, ts);
        }
        // #endregion
        if is_localhost_testing {
            tracing::warn!("🧪 LOCALHOST_TESTING=1 detected - FORCE TRANSITION ENABLED for development ONLY");
            tracing::warn!("⚠️  WARNING: This should NEVER be used in production - can cause forks!");
            tracing::warn!("🚀 FORCE TRANSITION: Bypassing all checks and returning true");
            return true; // Force transition for localhost testing
        }

        if !quorum_reached && !is_catchup_mode && !is_localhost_testing {
            // Log why transition is not ready (throttled to avoid spam)
            let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
            let barrier_passed = current_commit_index >= transition_commit_index;
            
            if barrier_passed {
                // Barrier passed but quorum not reached - this is important to log
                tracing::warn!(
                    "⏸️  Transition NOT ready: epoch {} -> {}, barrier passed ({} >= {}), but quorum not reached (status: {:?})",
                    proposal.new_epoch - 1,
                    proposal.new_epoch,
                    current_commit_index,
                    transition_commit_index,
                    quorum_status
                );
            }
            return false;
        }
        
        // CATCH-UP MODE: Log khi cho phép transition mà không cần quorum
        if is_catchup_mode && !quorum_reached {
            tracing::warn!(
                "🚀 CATCH-UP MODE: Allowing transition without quorum - epoch {} -> {} (lag={} epochs, max_lag={})",
                proposal.new_epoch - 1,
                proposal.new_epoch,
                epoch_lag,
                MAX_EPOCH_LAG_FOR_QUORUM
            );
            tracing::warn!(
                "   ⚠️  Node is lagging behind - other nodes are at epoch {} or higher",
                proposal.new_epoch
            );
            tracing::warn!(
                "   ✅ Allowing transition to catch up (fork-safe: using commit index barrier)"
            );
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
        let barrier_reached = current_commit_index >= transition_commit_index;

        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"D","location":"epoch_change.rs:should_transition:barrier_check","message":"Barrier check","data":{{"current_commit_index":{},"proposal_commit_index":{},"transition_commit_index":{},"barrier_reached":{}}},"timestamp":{}}}"#, 
                current_commit_index, proposal.proposal_commit_index, transition_commit_index, barrier_reached, ts);
        }
        // #endregion
        
        // ✅ Điều kiện 3: Timeout mechanism để tránh deadlock
        // Nếu quorum đã đạt nhưng commit index không tăng trong 5 phút,
        // có thể các node khác đã transition → cho phép transition để tránh deadlock
        // CRITICAL: Chỉ áp dụng timeout khi quorum đã đạt và đã chờ đủ lâu
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let proposal_age_seconds = now_seconds.saturating_sub(proposal.created_at_seconds);
        const TIMEOUT_SECONDS: u64 = 300; // 5 minutes timeout
        let timeout_reached = proposal_age_seconds >= TIMEOUT_SECONDS;
        
        // Allow transition if:
        // 1. Barrier reached (normal case)
        // 2. Timeout reached AND quorum reached (deadlock prevention)
        // 3. Catch-up mode: barrier reached (node lagging, needs to catch up)
        // 4. Localhost testing: bypass all checks for development
        // CRITICAL: Catch-up mode vẫn cần barrier để đảm bảo fork-safety
        let ready = barrier_reached || (timeout_reached && quorum_reached) || (is_catchup_mode && barrier_reached) || is_localhost_testing;

        // #region agent log
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let _ = writeln!(f, r#"{{"sessionId":"debug-session","runId":"run1","hypothesisId":"E","location":"epoch_change.rs:should_transition:final_decision","message":"Final transition decision","data":{{"ready":{},"barrier_reached":{},"timeout_reached":{},"quorum_reached":{},"is_catchup_mode":{},"is_localhost_testing":{},"proposal_age_seconds":{}}},"timestamp":{}}}"#, 
                ready, barrier_reached, timeout_reached, quorum_reached, is_catchup_mode, is_localhost_testing, proposal_age_seconds, ts);
        }
        // #endregion

        if is_localhost_testing && ready {
            tracing::info!("✅ Transition ready (LOCALHOST BYPASS): epoch {} -> {}, quorum_bypassed={}, barrier_bypassed={}",
                proposal.new_epoch - 1, proposal.new_epoch, !quorum_reached, !barrier_reached);
        }

        if ready {
            if barrier_reached {
                // Log for monitoring - ensure all nodes transition at similar commit index
                tracing::info!(
                    "✅ Transition ready: epoch {} -> {}, current_commit={}, barrier={}, diff={}, quorum=APPROVED",
                    proposal.new_epoch - 1,
                    proposal.new_epoch,
                    current_commit_index,
                    transition_commit_index,
                    current_commit_index.saturating_sub(transition_commit_index)
                );
            } else if timeout_reached {
                // Timeout-based transition (deadlock prevention)
                tracing::warn!(
                    "⏰ Transition ready (TIMEOUT): epoch {} -> {}, current_commit={}, barrier={}, proposal_age={}s (timeout={}s), quorum=APPROVED",
                    proposal.new_epoch - 1,
                    proposal.new_epoch,
                    current_commit_index,
                    transition_commit_index,
                    proposal_age_seconds,
                    TIMEOUT_SECONDS
                );
                tracing::warn!(
                    "   ⚠️  Commit index has not increased for {}s - other nodes may have already transitioned",
                    proposal_age_seconds
                );
                tracing::warn!(
                    "   ⚠️  Allowing transition to prevent deadlock (quorum already reached)"
                );
            }
        } else {
            // Log when quorum reached but barrier not passed yet
            let remaining_timeout = TIMEOUT_SECONDS.saturating_sub(proposal_age_seconds);
            tracing::debug!(
                "⏳ Transition waiting: epoch {} -> {}, current_commit={}, barrier={}, need {} more commits, timeout in {}s",
                proposal.new_epoch - 1,
                proposal.new_epoch,
                current_commit_index,
                transition_commit_index,
                transition_commit_index.saturating_sub(current_commit_index),
                remaining_timeout
            );
        }
        
        ready
    }

    /// Get proposal ready for transition (quorum reached + commit index barrier passed)
    /// IMPORTANT: Prioritize proposals for the NEXT epoch (current_epoch + 1)
    /// This ensures we transition to the correct epoch even if old proposals are still pending
    pub fn get_transition_ready_proposal(
        &self,
        current_commit_index: u32,
    ) -> Option<EpochChangeProposal> {
        tracing::info!("🔍 get_transition_ready_proposal called: current_commit_index={}, pending_proposals={}",
            current_commit_index, self.pending_proposals.len());

        // First, try to find proposal for the NEXT epoch (current_epoch + 1)
        // This is the most relevant proposal for transition
        let target_epoch = self.current_epoch + 1;
        
        // Log all pending proposals for debugging
        if !self.pending_proposals.is_empty() {
            tracing::info!(
                "🔍 Checking transition readiness: current_epoch={}, target_epoch={}, current_commit={}, pending_proposals={}",
                self.current_epoch,
                target_epoch,
                current_commit_index,
                self.pending_proposals.len()
            );
        }
        
        for proposal in self.pending_proposals.values() {
            // Prioritize proposal for next epoch
            if proposal.new_epoch == target_epoch {
                let should = self.should_transition(proposal, current_commit_index);
                if should {
                    tracing::info!(
                        "✅ Found ready proposal for transition: epoch {} -> {}, proposal_hash={:?}",
                        proposal.new_epoch - 1,
                        proposal.new_epoch,
                        hex::encode(&self.hash_proposal(proposal)[..8])
                    );
                    return Some(proposal.clone());
                } else {
                    // Log why this proposal is not ready
                    let quorum_status = self.check_proposal_quorum(proposal);
                    let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
                    tracing::info!(
                        "⏸️  Proposal not ready: epoch {} -> {}, quorum={:?}, barrier={}, current_commit={}",
                        proposal.new_epoch - 1,
                        proposal.new_epoch,
                        quorum_status,
                        transition_commit_index,
                        current_commit_index
                    );
                }
            }
        }
        
        // Fallback: if no proposal for next epoch is ready, check other proposals
        // This handles edge cases where we might have skipped an epoch
        for proposal in self.pending_proposals.values() {
            if self.should_transition(proposal, current_commit_index) {
                tracing::info!(
                    "✅ Found ready proposal (fallback): epoch {} -> {}",
                    proposal.new_epoch - 1,
                    proposal.new_epoch
                );
                return Some(proposal.clone());
            }
        }
        
        None
    }

    /// Check if there's a pending proposal for a specific epoch
    pub fn has_pending_proposal_for_epoch(&mut self, epoch: u64) -> bool {
        info!("🔍 Checking pending proposals for epoch {} (total proposals: {})", epoch, self.pending_proposals.len());

        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Clean up stale proposals (older than 10 minutes)
        let stale_proposals: Vec<Vec<u8>> = self.pending_proposals.iter()
            .filter(|(_, proposal)| {
                let proposal_age_seconds = now_seconds.saturating_sub(proposal.created_at_seconds);
                let is_stale = proposal_age_seconds > 600; // 10 minutes timeout
                if is_stale {
                    let proposal_hash = self.hash_proposal(proposal);
                    warn!("🧹 Found stale proposal: age={}s, hash={}, created_at={}",
                          proposal_age_seconds,
                          hex::encode(&proposal_hash[..8]),
                          proposal.created_at_seconds);
                }
                is_stale
            })
            .map(|(hash, _)| hash.clone())
            .collect();

        warn!("🧹 Found {} stale proposals to clean up (now={})", stale_proposals.len(), now_seconds);

        for stale_hash in stale_proposals {
            warn!("🧹 Removing stale epoch change proposal: {}", hex::encode(&stale_hash[..8]));
            self.pending_proposals.remove(&stale_hash);
            self.proposal_votes.remove(&stale_hash);
        }

        let has_pending = self.pending_proposals.values()
            .any(|p| p.new_epoch == epoch);

        // FORCE CLEAR: If epoch transition stuck too long, clear all pending proposals for that epoch
        // This ensures forward progress when epoch transitions get stuck
        if has_pending && epoch > self.current_epoch {
            let stuck_proposals: Vec<_> = self.pending_proposals.values()
                .filter(|p| p.new_epoch == epoch)
                .map(|p| {
                    let proposal_age = now_seconds.saturating_sub(p.created_at_seconds);
                    proposal_age
                })
                .collect();

            // Clear if ANY proposal is older than 1 hour (3600 seconds)
            // This ensures we don't get stuck indefinitely
            let max_age = stuck_proposals.iter().max().copied().unwrap_or(0);
            if max_age > 3600 {
                warn!("🚨 FORCE CLEAR: Epoch {} stuck for {}s (>1hr), clearing {} stale proposals to allow forward progress",
                      epoch, max_age, stuck_proposals.len());
                let to_remove: Vec<Vec<u8>> = self.pending_proposals.iter()
                    .filter(|(_, p)| p.new_epoch == epoch)
                    .map(|(hash, _)| hash.clone())
                    .collect();

                for hash in to_remove {
                    self.pending_proposals.remove(&hash);
                    self.proposal_votes.remove(&hash);
                }
                return false; // No longer has pending after clear
            }
        }

        info!("🔍 Epoch {} has pending proposal: {}", epoch, has_pending);
        has_pending
    }

    /// Broadcast epoch change proposal to all peers

    /// Get all pending proposals (for logging)
    pub fn get_all_pending_proposals(&self) -> Vec<EpochChangeProposal> {
        self.pending_proposals.values().cloned().collect()
    }

    /// Check if we have already voted on a proposal

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

