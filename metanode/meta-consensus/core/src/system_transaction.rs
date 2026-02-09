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
    /// SIMPLIFIED: Timestamp is NOT stored here - derive from boundary_block's block header
    EndOfEpoch {
        /// New epoch number
        new_epoch: Epoch,
        /// Boundary block (last block of previous epoch) - used to derive timestamp
        /// All nodes query block header at this index to get deterministic timestamp
        boundary_block: u64,
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
    /// Timestamp is NOT stored - derive from boundary_block's block header when needed
    pub fn end_of_epoch(new_epoch: Epoch, boundary_block: u64) -> Self {
        Self {
            kind: SystemTransactionKind::EndOfEpoch {
                new_epoch,
                boundary_block,
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
    /// Returns (new_epoch, boundary_block)
    pub fn as_end_of_epoch(&self) -> Option<(Epoch, u64)> {
        match &self.kind {
            SystemTransactionKind::EndOfEpoch {
                new_epoch,
                boundary_block,
            } => Some((*new_epoch, *boundary_block)),
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
                boundary_block,
            } => write!(
                f,
                "EndOfEpoch(new_epoch={}, boundary_block={})",
                new_epoch, boundary_block
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
