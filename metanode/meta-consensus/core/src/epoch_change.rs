// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

/// Proposal for epoch change
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochChangeProposal {
    /// New epoch number
    pub new_epoch: u64,
    /// Timestamp when new epoch starts (milliseconds since Unix epoch)
    pub new_epoch_timestamp_ms: u64,
    /// Commit index at which proposal was created
    pub proposal_commit_index: u32,
    /// Index of authority proposing the change
    pub proposer_value: u32,
    /// Signature of the proposal
    pub signature_bytes: Vec<u8>,
    /// Timestamp when proposal was created (seconds since Unix epoch)
    pub created_at_seconds: u64,
}

/// Vote on an epoch change proposal
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochChangeVote {
    /// Hash of the proposal being voted on
    pub proposal_hash: Vec<u8>,
    /// Index of authority casting the vote
    pub voter_value: u32,
    /// Whether the vote is in favor (true) or against (false)
    pub approve: bool,
    /// Signature of the vote
    pub signature_bytes: Vec<u8>,
}

impl EpochChangeProposal {
    /// Hash the proposal for signing/verification
    pub fn hash(&self) -> Vec<u8> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.new_epoch.hash(&mut hasher);
        self.new_epoch_timestamp_ms.hash(&mut hasher);
        self.proposal_commit_index.hash(&mut hasher);
        self.proposer_value.hash(&mut hasher);
        hasher.finish().to_le_bytes().to_vec()
    }
}

impl EpochChangeVote {
    /// Hash the vote for signing/verification
    pub fn hash(&self) -> Vec<u8> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.proposal_hash.hash(&mut hasher);
        self.voter_value.hash(&mut hasher);
        self.approve.hash(&mut hasher);
        hasher.finish().to_le_bytes().to_vec()
    }
}
