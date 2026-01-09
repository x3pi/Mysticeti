// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

/// Calculate deterministic global execution index (Checkpoint Sequence Number)
/// This ensures all nodes compute the same value from consensus state
///
/// Formula:
/// - Epoch 0: global_exec_index = commit_index (commit_index starts from 1, so global_exec_index starts from 1)
/// - Epoch N (N > 0): global_exec_index = last_global_exec_index + commit_index
///
/// IMPORTANT: In Mysticeti, commit_index starts from 1 in every epoch
/// (`CommitProcessor::next_expected_index` starts at 1). Therefore, epoch N's first commit
/// (commit_index=1) must map to global_exec_index = last_global_exec_index + 1 for continuous,
/// strictly increasing execution order across epochs.
///
/// This is similar to Sui's Checkpoint Sequence Number which increases continuously
/// across epochs without resetting.
/// This ensures all nodes compute the same value from consensus state
/// 
/// Formula:
/// - Epoch 0: global_exec_index = commit_index (commit_index starts from 1, so global_exec_index starts from 1)
/// - Epoch N (N > 0): global_exec_index = last_global_exec_index + commit_index
/// 
/// IMPORTANT: In Mysticeti, commit_index starts from 1 in every epoch
/// (`CommitProcessor::next_expected_index` starts at 1). Therefore, epoch N's first commit
/// (commit_index=1) must map to global_exec_index = last_global_exec_index + 1 for continuous,
/// strictly increasing execution order across epochs.
/// 
/// This is similar to Sui's Checkpoint Sequence Number which increases continuously
/// across epochs without resetting.
pub fn calculate_global_exec_index(
    epoch: u64,
    commit_index: u32,
    last_global_exec_index: u64,
) -> u64 {
    if epoch == 0 {
        // Epoch 0: commit_index starts from 1, so global_exec_index starts from 1
        // commit_index=1 → global_exec_index=1 (first block is block 1)
        // commit_index=2 → global_exec_index=2, etc.

        // SPECIAL CASE: If last_global_exec_index is much higher than commit_index,
        // it means we're synchronizing with an existing chain state.
        // Use last_global_exec_index as base to continue from the correct point.
        if last_global_exec_index > commit_index as u64 * 10 {
            // Synchronization case: continue from last_global_exec_index
            last_global_exec_index + commit_index as u64
        } else {
            // Normal genesis case: start from commit_index
            commit_index as u64
        }
    } else {
        // Epoch N: commit_index starts from 1 → first global_exec_index is last_global_exec_index + 1
        // Example:
        // - Epoch 0 ends at global_exec_index=1276
        // - Epoch 1, commit_index=1 → global_exec_index = 1276 + 1 = 1277
        last_global_exec_index + commit_index as u64
    }
}

/// Checkpoint data structures for Sui-style epoch transitions
/// Similar to Sui's CheckpointSummary but simplified for MetaNode

use serde::{Deserialize, Serialize};

/// Simplified Checkpoint for MetaNode (focused on epoch transition)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Checkpoint sequence number (total blocks processed)
    pub sequence_number: u64,
    /// Epoch this checkpoint belongs to
    pub epoch: u64,
    /// Commit index that created this checkpoint
    pub commit_index: u32,
    /// Timestamp when checkpoint was created
    pub timestamp_ms: u64,
    /// Total blocks in this checkpoint
    pub block_count: u32,
    /// End of epoch data (only present for last checkpoint of epoch)
    pub end_of_epoch_data: Option<EndOfEpochData>,
}

/// End of epoch data (Sui-style)
/// Triggers epoch transition when present
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EndOfEpochData {
    /// Next epoch committee (triggers epoch transition)
    pub next_epoch_committee: Vec<(consensus_config::AuthorityIndex, u64)>,
    /// Next epoch protocol version
    pub next_epoch_protocol_version: u64,
    /// Epoch start timestamp
    pub epoch_start_timestamp_ms: u64,
}

/// Checkpoint Manager - Simplified version for MetaNode
/// Manages checkpoint creation and epoch transition triggers
pub struct CheckpointManager {
    /// Current checkpoints (in memory for now, can be persisted later)
    checkpoints: std::sync::RwLock<std::collections::BTreeMap<u64, Checkpoint>>,
    /// Current epoch
    current_epoch: std::sync::atomic::AtomicU64,
    /// Last checkpoint sequence number processed
    last_sequence_number: std::sync::atomic::AtomicU64,
    /// Total blocks processed (for sequence number calculation)
    total_blocks_processed: std::sync::atomic::AtomicU64,
    /// Blocks per epoch configuration
    blocks_per_epoch: u64,
    /// Node authority index
    own_index: consensus_config::AuthorityIndex,
}

impl CheckpointManager {
    pub fn new(blocks_per_epoch: u64, own_index: consensus_config::AuthorityIndex) -> Self {
        Self {
            checkpoints: std::sync::RwLock::new(std::collections::BTreeMap::new()),
            current_epoch: std::sync::atomic::AtomicU64::new(0),
            last_sequence_number: std::sync::atomic::AtomicU64::new(0),
            total_blocks_processed: std::sync::atomic::AtomicU64::new(0),
            blocks_per_epoch,
            own_index,
        }
    }

    /// Create checkpoint from committed subdag
    pub async fn create_checkpoint(
        &self,
        commit_index: u32,
        _global_exec_index: u64, // Not used anymore
        block_count: u32,
    ) -> Result<Checkpoint, anyhow::Error> {
        // Update total blocks processed and get sequence number
        let previous_total = self.total_blocks_processed.fetch_add(block_count as u64, std::sync::atomic::Ordering::SeqCst);
        let sequence_number = previous_total + block_count as u64;

        let epoch = self.current_epoch.load(std::sync::atomic::Ordering::SeqCst);

        // Check if this should be an end-of-epoch checkpoint
        let end_of_epoch_data = self.should_create_end_of_epoch_checkpoint(sequence_number)
            .await?;

        let checkpoint = Checkpoint {
            sequence_number,
            epoch,
            commit_index,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            block_count, // Changed from transaction_count
            end_of_epoch_data,
        };

        // Store checkpoint
        {
            let mut checkpoints = self.checkpoints.write().unwrap();
            checkpoints.insert(sequence_number, checkpoint.clone());
        }

        // Update last sequence number
        self.last_sequence_number.store(sequence_number, std::sync::atomic::Ordering::SeqCst);

        tracing::info!("✅ Created checkpoint {} (epoch={}, commit_index={}, blocks={})",
            sequence_number, epoch, commit_index, block_count);

        Ok(checkpoint)
    }

    /// Check if we should create end-of-epoch checkpoint (Sui-style logic)
    async fn should_create_end_of_epoch_checkpoint(
        &self,
        sequence_number: u64,
    ) -> Result<Option<EndOfEpochData>, anyhow::Error> {
        let current_epoch = self.current_epoch.load(std::sync::atomic::Ordering::SeqCst);
        // Epoch boundary = (current_epoch + 1) * blocks_per_epoch
        // Since sequence_number is global and never resets
        let epoch_boundary = (current_epoch + 1) * self.blocks_per_epoch as u64;

        tracing::info!("🔍 Checkpoint {}: current_epoch={}, boundary={}, blocks_per_epoch={}",
            sequence_number, current_epoch, epoch_boundary, self.blocks_per_epoch);

        // If this checkpoint reaches the epoch boundary
        if sequence_number >= epoch_boundary {
            // Create end-of-epoch data (similar to Sui's next_epoch_committee)
            let end_of_epoch_data = EndOfEpochData {
                next_epoch_committee: self.get_next_epoch_committee().await?,
                next_epoch_protocol_version: 1, // Simplified
                epoch_start_timestamp_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            };

            tracing::info!("🎯 Checkpoint {} is end-of-epoch for epoch {} ({} blocks)", sequence_number, current_epoch, self.blocks_per_epoch);
            Ok(Some(end_of_epoch_data))
        } else {
            Ok(None)
        }
    }

    /// Get next epoch committee (simplified - preserve current committee)
    async fn get_next_epoch_committee(&self) -> Result<Vec<(consensus_config::AuthorityIndex, u64)>, anyhow::Error> {
        // For now, return current committee
        // In real implementation, this would query the committee store
        // and potentially apply validator changes
        Ok(vec![
            (consensus_config::AuthorityIndex::new_for_test(0), 1000),
            (consensus_config::AuthorityIndex::new_for_test(1), 1000),
            (consensus_config::AuthorityIndex::new_for_test(2), 1000),
            (consensus_config::AuthorityIndex::new_for_test(3), 1000),
        ])
    }

    /// Check if checkpoint triggers epoch transition (Sui-style)
    pub async fn checkpoint_triggers_epoch_transition(&self, checkpoint: &Checkpoint) -> bool {
        checkpoint.end_of_epoch_data.is_some()
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> u64 {
        self.current_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Advance to next epoch
    pub fn advance_epoch(&self) {
        let current = self.current_epoch.load(std::sync::atomic::Ordering::SeqCst);
        self.current_epoch.store(current + 1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Get last checkpoint sequence number
    pub fn last_checkpoint_seq(&self) -> u64 {
        self.last_sequence_number.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_global_exec_index_epoch_0() {
        // Epoch 0: global_exec_index = commit_index (commit_index starts from 1, so global_exec_index starts from 1)
        assert_eq!(calculate_global_exec_index(0, 0, 0), 0); // commit_index=0 → global_exec_index=0 (edge case)
        assert_eq!(calculate_global_exec_index(0, 1, 0), 1); // commit_index=1 → global_exec_index=1 (first block is block 1)
        assert_eq!(calculate_global_exec_index(0, 2, 0), 2); // commit_index=2 → global_exec_index=2
        assert_eq!(calculate_global_exec_index(0, 100, 0), 100); // commit_index=100 → global_exec_index=100
    }

    #[test]
    fn test_calculate_global_exec_index_epoch_1() {
        // Epoch 1: global_exec_index = last_global_exec_index + commit_index
        // Assume epoch 0 ended at commit_index 100, so last_global_exec_index = 100
        // Epoch 1, commit_index=1 → global_exec_index = 100 + 1 = 101
        assert_eq!(calculate_global_exec_index(1, 0, 100), 100); // commit_index=0 (edge case; not used by consensus)
        assert_eq!(calculate_global_exec_index(1, 1, 100), 101);
        assert_eq!(calculate_global_exec_index(1, 50, 100), 150);
    }

    #[test]
    fn test_calculate_global_exec_index_epoch_2() {
        // Epoch 2: continue from epoch 1
        // Assume epoch 1 ended at commit_index 50, so last_global_exec_index = 150 (100 + 50)
        // Epoch 2, commit_index=1 → global_exec_index = 150 + 1 = 151
        assert_eq!(calculate_global_exec_index(2, 0, 150), 150); // edge case (not used by consensus)
        assert_eq!(calculate_global_exec_index(2, 1, 150), 151);
        assert_eq!(calculate_global_exec_index(2, 25, 150), 175);
    }

    #[test]
    fn test_deterministic_across_nodes() {
        // All nodes with same epoch, commit_index, last_global_exec_index
        // should compute the same global_exec_index
        let epoch = 5;
        let commit_index = 42;
        let last_global_exec_index = 1000;
        
        let result1 = calculate_global_exec_index(epoch, commit_index, last_global_exec_index);
        let result2 = calculate_global_exec_index(epoch, commit_index, last_global_exec_index);
        let result3 = calculate_global_exec_index(epoch, commit_index, last_global_exec_index);
        
        assert_eq!(result1, result2);
        assert_eq!(result2, result3);
        assert_eq!(result1, 1042); // 1000 + 42
    }
}

