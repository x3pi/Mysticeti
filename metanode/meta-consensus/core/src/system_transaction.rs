// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use consensus_config::Epoch;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Validator info for epoch boundary (serializable)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochValidatorInfo {
    pub name: String,
    pub address: String, // Multiaddr format
    pub stake: u64,
    pub authority_key: Vec<u8>, // BLS public key bytes
    pub protocol_key: Vec<u8>,  // Ed25519 protocol key bytes
    pub network_key: Vec<u8>,   // Ed25519 network key bytes
}

/// System transaction types (similar to Sui's EndOfEpochTransactionKind)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SystemTransactionKind {
    /// End of epoch transaction - triggers epoch transition
    /// This replaces the Proposal/Vote/Quorum mechanism
    EndOfEpoch {
        /// New epoch number
        new_epoch: Epoch,
        /// New epoch start timestamp in milliseconds
        new_epoch_timestamp_ms: u64,
        /// Commit index when this transaction was created (for fork-safety)
        commit_index: u32,
    },
    /// Epoch boundary data - included in genesis block of each new epoch
    /// Allows late-joining nodes to recover boundary data by syncing blocks
    EpochBoundary {
        /// Epoch number this boundary data is for
        epoch: Epoch,
        /// Epoch start timestamp in milliseconds (for genesis hash consistency)
        epoch_start_timestamp_ms: u64,
        /// Last block of previous epoch (boundary point)
        boundary_block: u64,
        /// Validators active in this epoch
        validators: Vec<EpochValidatorInfo>,
    },
}

/// System transaction that is automatically included in blocks
/// Similar to Sui's EndOfEpochTransaction, but simpler
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemTransaction {
    pub kind: SystemTransactionKind,
}

impl SystemTransaction {
    /// Create a new EndOfEpoch system transaction
    pub fn end_of_epoch(new_epoch: Epoch, new_epoch_timestamp_ms: u64, commit_index: u32) -> Self {
        Self {
            kind: SystemTransactionKind::EndOfEpoch {
                new_epoch,
                new_epoch_timestamp_ms,
                commit_index,
            },
        }
    }

    /// Create a new EpochBoundary system transaction
    pub fn epoch_boundary(
        epoch: Epoch,
        epoch_start_timestamp_ms: u64,
        boundary_block: u64,
        validators: Vec<EpochValidatorInfo>,
    ) -> Self {
        Self {
            kind: SystemTransactionKind::EpochBoundary {
                epoch,
                epoch_start_timestamp_ms,
                boundary_block,
                validators,
            },
        }
    }

    /// Serialize to bytes (BCS format)
    pub fn to_bytes(&self) -> Result<Vec<u8>, bcs::Error> {
        bcs::to_bytes(self)
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, bcs::Error> {
        bcs::from_bytes(bytes)
    }

    /// Check if this is an EndOfEpoch transaction
    pub fn is_end_of_epoch(&self) -> bool {
        matches!(self.kind, SystemTransactionKind::EndOfEpoch { .. })
    }

    /// Check if this is an EpochBoundary transaction
    pub fn is_epoch_boundary(&self) -> bool {
        matches!(self.kind, SystemTransactionKind::EpochBoundary { .. })
    }

    /// Extract EndOfEpoch data if this is an EndOfEpoch transaction
    pub fn as_end_of_epoch(&self) -> Option<(Epoch, u64, u32)> {
        match &self.kind {
            SystemTransactionKind::EndOfEpoch {
                new_epoch,
                new_epoch_timestamp_ms,
                commit_index,
            } => Some((*new_epoch, *new_epoch_timestamp_ms, *commit_index)),
            _ => None,
        }
    }

    /// Extract EpochBoundary data if this is an EpochBoundary transaction
    pub fn as_epoch_boundary(&self) -> Option<(Epoch, u64, u64, &Vec<EpochValidatorInfo>)> {
        match &self.kind {
            SystemTransactionKind::EpochBoundary {
                epoch,
                epoch_start_timestamp_ms,
                boundary_block,
                validators,
            } => Some((
                *epoch,
                *epoch_start_timestamp_ms,
                *boundary_block,
                validators,
            )),
            _ => None,
        }
    }
}

impl fmt::Display for SystemTransaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            SystemTransactionKind::EndOfEpoch {
                new_epoch,
                new_epoch_timestamp_ms,
                commit_index,
            } => write!(
                f,
                "EndOfEpoch(new_epoch={}, timestamp_ms={}, commit_index={})",
                new_epoch, new_epoch_timestamp_ms, commit_index
            ),
            SystemTransactionKind::EpochBoundary {
                epoch,
                epoch_start_timestamp_ms,
                boundary_block,
                validators,
            } => write!(
                f,
                "EpochBoundary(epoch={}, timestamp_ms={}, boundary_block={}, validators={})",
                epoch,
                epoch_start_timestamp_ms,
                boundary_block,
                validators.len()
            ),
        }
    }
}

/// Helper to identify system transactions in transaction data
#[allow(dead_code)] // May be used in future
pub fn is_system_transaction(data: &[u8]) -> bool {
    // Try to deserialize as SystemTransaction
    // If it succeeds, it's a system transaction
    SystemTransaction::from_bytes(data).is_ok()
}

/// Extract system transaction from transaction data
pub fn extract_system_transaction(data: &[u8]) -> Option<SystemTransaction> {
    SystemTransaction::from_bytes(data).ok()
}
