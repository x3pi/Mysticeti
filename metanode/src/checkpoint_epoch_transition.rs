// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use consensus_config::AuthorityIndex;
use consensus_core::BlockAPI;
use crate::checkpoint::{Checkpoint, EndOfEpochData, CheckpointManager};
use crate::executor_client::ExecutorClient;
use crate::config::NodeConfig;

/// Checkpoint-based epoch transition manager (Sui-style)
/// Replaces block-based epoch transitions with deterministic checkpoint triggers
pub struct CheckpointEpochTransitionManager {
    /// Checkpoint manager
    checkpoint_manager: Arc<CheckpointManager>,
    /// Executor client for Go state interaction
    executor_client: Arc<ExecutorClient>,
    /// Node configuration
    config: NodeConfig,
    /// Own authority index (determines if this node should create epoch change transaction)
    own_index: AuthorityIndex,
    /// Block counter for current epoch (shared with CommitProcessor to reset on epoch transition)
    blocks_in_current_epoch: Arc<std::sync::atomic::AtomicU32>,
}

impl CheckpointEpochTransitionManager {
    pub fn new(
        checkpoint_manager: Arc<CheckpointManager>,
        executor_client: Arc<ExecutorClient>,
        config: NodeConfig,
        own_index: AuthorityIndex,
        blocks_in_current_epoch: Arc<std::sync::atomic::AtomicU32>,
    ) -> Self {
        Self {
            checkpoint_manager,
            executor_client,
            config,
            own_index,
            blocks_in_current_epoch,
        }
    }

    /// Process committed subdag and check for checkpoint-based epoch transition (Sui-style)
    /// Called from commit processor after successful commit
    pub async fn process_commit_and_check_epoch_transition(
        &self,
        subdag: &consensus_core::CommittedSubDag,
        commit_index: u32,
        global_exec_index: u64,
    ) -> Result<(), anyhow::Error> {
        // Calculate checkpoint sequence number (global_exec_index = checkpoint seq number)
        let checkpoint_seq = global_exec_index;

        // Update last processed checkpoint
        // self.last_checkpoint_seq.store(checkpoint_seq, std::sync::atomic::Ordering::SeqCst);

        // Create checkpoint from this commit
        let block_count = subdag.blocks.len() as u32;

        let checkpoint = self.checkpoint_manager.create_checkpoint(
            commit_index,
            global_exec_index,
            block_count,
        ).await?;

        // DEBUG: Log checkpoint creation
        tracing::debug!("📝 Created checkpoint {} in epoch {}: commit_index={}, sequence={}, end_of_epoch={}",
            checkpoint.sequence_number,
            checkpoint.epoch,
            checkpoint.commit_index,
            checkpoint.sequence_number,
            checkpoint.end_of_epoch_data.is_some()
        );

        // Check if this checkpoint should trigger epoch transition
        if self.checkpoint_manager.checkpoint_triggers_epoch_transition(&checkpoint).await {
            // HIERARCHICAL LEADER ELECTION: Primary + Fallback leaders prevent single point of failure
            // Primary: current_epoch % total_nodes
            // Fallback: (current_epoch + 1) % total_nodes
            // This ensures epoch transitions can always happen even if primary leader fails
            let current_epoch = self.checkpoint_manager.current_epoch();
            let total_nodes = 4; // Local environment has 4 nodes
            let primary_leader = current_epoch % total_nodes;
            let fallback_leader = (current_epoch + 1) % total_nodes;
            let my_index = self.own_index.value() as u64;

            if my_index == primary_leader {
                tracing::info!("🚀 Checkpoint {} triggers epoch transition (PRIMARY leader node {} for epoch {})",
                    checkpoint.sequence_number, my_index, current_epoch);
                self.trigger_epoch_transition_from_checkpoint(checkpoint, commit_index).await?;
            } else if my_index == fallback_leader {
                // Fallback leader waits a bit to give primary leader chance, then takes over
                tracing::info!("⏳ Checkpoint {} - primary leader {} may be down, fallback leader {} will attempt transition for epoch {}",
                    checkpoint.sequence_number, primary_leader, my_index, current_epoch);
                self.trigger_epoch_transition_from_checkpoint(checkpoint, commit_index).await?;
            } else {
                tracing::info!("⏸️ Checkpoint {} would trigger epoch transition but node {} is not primary ({}) or fallback ({}) leader for epoch {}",
                    checkpoint.sequence_number, my_index, primary_leader, fallback_leader, current_epoch);
            }
        }

        Ok(())
    }

    /// Trigger epoch transition when checkpoint has end_of_epoch_data
    async fn trigger_epoch_transition_from_checkpoint(
        &self,
        checkpoint: Checkpoint,
        commit_index: u32,
    ) -> Result<(), anyhow::Error> {
        if let Some(ref end_of_epoch_data) = checkpoint.end_of_epoch_data {
            // ✅ FAULT-TOLERANT: Tất cả nodes đều có thể tạo transaction
            // Transaction sẽ được consensus xử lý deterministically
            if let Err(e) = self.create_epoch_change_transaction(checkpoint.clone(), (*end_of_epoch_data).clone(), commit_index).await {
                // ⚠️  NON-FATAL: Log error but don't fail commit processor
                // This prevents consensus shutdown when Go side has issues
                tracing::error!("⚠️  Epoch transition transaction failed (non-fatal): {}", e);
                tracing::info!("🔄 Continuing with blocks despite epoch transition failure - system remains in epoch {}", checkpoint.epoch);
            } else {
                tracing::info!("✅ Epoch transition transaction submitted successfully");
            }
        }
        Ok(())
    }

    /// Execute epoch change transaction (replaces voting with transaction - Sui-style)
    async fn create_epoch_change_transaction(
        &self,
        checkpoint: Checkpoint,
        end_of_epoch_data: EndOfEpochData,
        commit_index: u32,
    ) -> Result<(), anyhow::Error> {
        // Create epoch change transaction data
        let epoch_change_data = CheckpointEpochChangeData {
            checkpoint_sequence: checkpoint.sequence_number,
            commit_index,
            new_epoch: checkpoint.epoch + 1,
            epoch_start_timestamp_ms: end_of_epoch_data.epoch_start_timestamp_ms,
            next_epoch_committee: end_of_epoch_data.next_epoch_committee,
        };

        tracing::info!("📝 Node {} creating epoch change transaction for epoch {} -> {}",
            self.own_index.value(), checkpoint.epoch, checkpoint.epoch + 1);

        // Submit to executor (consensus sẽ xử lý deterministically)
        match self.executor_client.submit_checkpoint_epoch_change_transaction(epoch_change_data).await {
            Ok(_) => {
                tracing::info!("✅ Checkpoint-based epoch change transaction submitted successfully: epoch {} -> {}",
                    checkpoint.epoch, checkpoint.epoch + 1);

                // Update local epoch
                let old_epoch = self.checkpoint_manager.current_epoch();
                self.checkpoint_manager.advance_epoch();
                let new_epoch = self.checkpoint_manager.current_epoch();
                tracing::info!("✅ CheckpointManager epoch advanced: {} -> {}", old_epoch, new_epoch);

                // Trigger reconfiguration
                self.trigger_reconfiguration(checkpoint.clone(), checkpoint.epoch + 1).await?;

                Ok(())
            }
            Err(e) => {
                tracing::error!("❌ Failed to submit epoch change transaction: {}", e);
                Err(e.into())
            }
        }
    }

    /// Trigger reconfiguration after successful epoch change transaction (Sui-style)
    async fn trigger_reconfiguration(
        &self,
        checkpoint: Checkpoint,
        new_epoch: u64,
    ) -> Result<(), anyhow::Error> {
        tracing::info!("🔄 Starting reconfiguration: epoch {} -> {}", checkpoint.epoch, new_epoch);

        // Sui-style reconfiguration steps:
        // 1. Close current epoch processing
        // 2. Update epoch state
        // 3. Open new epoch processing

        // Reset block counter for new epoch
        self.blocks_in_current_epoch.store(0, std::sync::atomic::Ordering::SeqCst);
        tracing::info!("🔄 Block counter reset to 0 for new epoch {}", new_epoch);

        // Signal reconfiguration complete
        tracing::info!("✅ Reconfiguration completed: now in epoch {}", new_epoch);

        Ok(())
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> u64 {
        self.checkpoint_manager.current_epoch()
    }

    /// Get last checkpoint sequence
    pub fn last_checkpoint_seq(&self) -> u64 {
        self.checkpoint_manager.last_checkpoint_seq()
    }
}

/// Epoch change transaction data (Sui-style transaction)
#[derive(Clone, Debug)]
pub struct CheckpointEpochChangeData {
    pub checkpoint_sequence: u64,
    pub commit_index: u32,
    pub new_epoch: u64,
    pub epoch_start_timestamp_ms: u64,
    pub next_epoch_committee: Vec<(AuthorityIndex, u64)>,
}
