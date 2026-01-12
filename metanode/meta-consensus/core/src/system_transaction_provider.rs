// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use crate::system_transaction::SystemTransaction;
use consensus_config::Epoch;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Provider for system transactions (similar to Sui's EndOfEpochTransaction provider)
/// This replaces the Proposal/Vote/Quorum mechanism with system transactions
pub trait SystemTransactionProvider: Send + Sync {
    /// Get system transactions to include in the next block
    /// Returns None if no system transaction should be included
    fn get_system_transactions(&self, current_epoch: Epoch, current_commit_index: u32) -> Option<Vec<SystemTransaction>>;
}

/// Default implementation that checks if epoch transition is needed
pub struct DefaultSystemTransactionProvider {
    /// Current epoch
    current_epoch: Arc<RwLock<Epoch>>,
    /// Epoch duration in seconds
    epoch_duration_seconds: u64,
    /// Epoch start timestamp in milliseconds
    epoch_start_timestamp_ms: Arc<RwLock<u64>>,
    /// Time-based epoch change enabled
    time_based_enabled: bool,
    /// Last commit index where we checked for epoch change
    last_checked_commit_index: Arc<RwLock<u32>>,
    /// Commit index buffer (number of commits to wait after detecting system transaction)
    /// Default: 100 commits for high commit rate systems (was 10)
    /// With commit rate 200 commits/s, 100 commits = 500ms (safer than 10 commits = 50ms)
    commit_index_buffer: u32,
}

impl DefaultSystemTransactionProvider {
    /// Create a new provider with default buffer (100 commits)
    pub fn new(
        current_epoch: Epoch,
        epoch_duration_seconds: u64,
        epoch_start_timestamp_ms: u64,
        time_based_enabled: bool,
    ) -> Self {
        Self::new_with_buffer(
            current_epoch,
            epoch_duration_seconds,
            epoch_start_timestamp_ms,
            time_based_enabled,
            100, // Default: 100 commits (increased from 10 for high commit rate safety)
        )
    }

    /// Create a new provider with custom commit index buffer
    /// 
    /// # Arguments
    /// * `commit_index_buffer` - Number of commits to wait after detecting system transaction
    ///   before triggering epoch transition. 
    ///   - For low commit rate (<10 commits/s): 10-20 commits is sufficient
    ///   - For medium commit rate (10-100 commits/s): 50-100 commits recommended
    ///   - For high commit rate (>100 commits/s): 100-200 commits recommended
    ///   - With 200 commits/s, 100 commits = 500ms (safer than 10 commits = 50ms)
    pub fn new_with_buffer(
        current_epoch: Epoch,
        epoch_duration_seconds: u64,
        epoch_start_timestamp_ms: u64,
        time_based_enabled: bool,
        commit_index_buffer: u32,
    ) -> Self {
        Self {
            current_epoch: Arc::new(RwLock::new(current_epoch)),
            epoch_duration_seconds,
            epoch_start_timestamp_ms: Arc::new(RwLock::new(epoch_start_timestamp_ms)),
            time_based_enabled,
            last_checked_commit_index: Arc::new(RwLock::new(0)),
            commit_index_buffer,
        }
    }

    /// Update current epoch (called after epoch transition)
    pub async fn update_epoch(&self, new_epoch: Epoch, new_timestamp_ms: u64) {
        *self.current_epoch.write().await = new_epoch;
        *self.epoch_start_timestamp_ms.write().await = new_timestamp_ms;
        *self.last_checked_commit_index.write().await = 0;
    }

    /// Check if epoch transition should be triggered
    fn should_trigger_epoch_change(&self, current_commit_index: u32) -> bool {
        if !self.time_based_enabled {
            return false;
        }

        // Only check once per commit index to avoid spam
        let last_checked = *self.last_checked_commit_index.blocking_read();
        if current_commit_index <= last_checked {
            return false;
        }

        // Check if enough time has elapsed
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        let epoch_start = *self.epoch_start_timestamp_ms.blocking_read();
        let elapsed_seconds = (now_ms - epoch_start) / 1000;
        
        let should_trigger = elapsed_seconds >= self.epoch_duration_seconds;
        
        if should_trigger {
            info!(
                "⏰ SystemTransactionProvider: Epoch change triggered - epoch={}, elapsed={}s, duration={}s, commit_index={}",
                *self.current_epoch.blocking_read(),
                elapsed_seconds,
                self.epoch_duration_seconds,
                current_commit_index
            );
        }
        
        should_trigger
    }
}

impl SystemTransactionProvider for DefaultSystemTransactionProvider {
    fn get_system_transactions(&self, current_epoch: Epoch, current_commit_index: u32) -> Option<Vec<SystemTransaction>> {
        // Update last checked commit index
        {
            let mut last_checked = self.last_checked_commit_index.blocking_write();
            if current_commit_index > *last_checked {
                *last_checked = current_commit_index;
            }
        }

        // Check if epoch transition should be triggered
        if !self.should_trigger_epoch_change(current_commit_index) {
            return None;
        }

        // Create EndOfEpoch system transaction
        // CRITICAL FORK-SAFETY: Use deterministic values only
        // - new_epoch: current_epoch + 1 (deterministic)
        // - new_epoch_timestamp_ms: epoch_start + epoch_duration (deterministic, not SystemTime::now())
        // - transition_commit_index: current_commit_index + 10 (deterministic, but may differ between nodes)
        let new_epoch = current_epoch + 1;
        
        // FORK-SAFETY FIX: Use deterministic timestamp calculation
        // Instead of SystemTime::now(), use epoch_start + epoch_duration
        // This ensures all nodes calculate the same timestamp
        let epoch_start = *self.epoch_start_timestamp_ms.blocking_read();
        let new_epoch_timestamp_ms = epoch_start + (self.epoch_duration_seconds * 1000);

        // FORK-SAFETY WARNING: transition_commit_index may differ between nodes
        // if they have different current_commit_index values.
        // This is acceptable because:
        // 1. System transaction will be included in a committed block
        // 2. All nodes will see the same system transaction in the committed block
        // 3. Transition happens when commit_index >= transition_commit_index (from the committed block)
        // However, to be extra safe, we should use the commit_index from the committed block
        // that contains the system transaction, not the current_commit_index when creating it.
        // 
        // For now, we use current_commit_index + 10, but the actual transition_commit_index
        // should be read from the committed block that contains this system transaction.
        // 
        // SAFETY: Use checked_add to handle overflow explicitly
        // If commit_index is too large (near u32::MAX), we use u32::MAX - 1 to ensure
        // transition can still be triggered, but log a warning.
        // 
        // BUFFER SAFETY: Increased from 10 to configurable buffer (default 100) for high commit rate systems.
        // With commit rate 200 commits/s:
        // - 10 commits = 50ms (not safe for network propagation)
        // - 100 commits = 500ms (safer, allows network delay and processing time)
        let transition_commit_index = current_commit_index
            .checked_add(self.commit_index_buffer)
            .unwrap_or_else(|| {
                warn!(
                    "⚠️ [FORK-SAFETY] commit_index {} quá lớn (gần u32::MAX), không thể cộng buffer {}. \
                     Sử dụng u32::MAX - 1 làm transition_commit_index. \
                     Điều này có thể gây vấn đề nếu commit_index tiếp tục tăng. \
                     Cân nhắc reset commit_index hoặc tăng epoch duration.",
                    current_commit_index, self.commit_index_buffer
                );
                u32::MAX - 1
            });

        info!(
            "📝 SystemTransactionProvider: Creating EndOfEpoch transaction - epoch {} -> {}, commit_index={}, transition_commit_index={}",
            current_epoch,
            new_epoch,
            current_commit_index,
            transition_commit_index
        );

        let system_tx = SystemTransaction::end_of_epoch(
            new_epoch,
            new_epoch_timestamp_ms,
            transition_commit_index,
        );

        Some(vec![system_tx])
    }
}
