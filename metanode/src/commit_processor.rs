// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI};
use mysten_metrics::monitored_mpsc::UnboundedReceiver;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use tracing::{info, warn};

use crate::checkpoint::calculate_global_exec_index;
use crate::executor_client::ExecutorClient;
use crate::tx_hash::calculate_transaction_hash_hex;

/// Commit processor that ensures commits are executed in order
pub struct CommitProcessor {
    receiver: UnboundedReceiver<CommittedSubDag>,
    next_expected_index: u32, // CommitIndex is u32
    pending_commits: BTreeMap<u32, CommittedSubDag>,
    /// Optional callback to notify commit index updates (for epoch transition)
    commit_index_callback: Option<Arc<dyn Fn(u32) + Send + Sync>>,
    /// Current epoch (for deterministic global_exec_index calculation)
    current_epoch: u64,
    /// Last global execution index from previous epoch (for deterministic calculation)
    last_global_exec_index: u64,
    /// Optional executor client to send blocks to Go executor
    executor_client: Option<Arc<ExecutorClient>>,
    /// Transition barrier commit index (for preventing duplicate global_exec_index)
    /// If set, commits with commit_index > transition_barrier will be recalculated with new epoch
    /// This prevents duplicate global_exec_index between epochs
    /// Uses AtomicU32 for thread-safe access (can be set from epoch transition while CommitProcessor is running)
    transition_barrier: Option<Arc<AtomicU32>>,
    /// Global exec index at barrier (for commits past barrier)
    /// If set, commits past barrier will use: global_exec_index_at_barrier + (commit_index - barrier_value)
    /// Uses AtomicU64 for thread-safe access
    global_exec_index_at_barrier: Option<Arc<AtomicU64>>,
    /// Queue for transactions that must be retried in the next epoch.
    ///
    /// When we skip commits past barrier (to avoid duplicate `global_exec_index`), any transactions found
    /// inside those commits must NOT be lost. We re-queue them here so that `ConsensusNode` can submit
    /// them deterministically after epoch transition.
    pending_transactions_queue: Option<Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>>,
}

impl CommitProcessor {
    pub fn new(receiver: UnboundedReceiver<CommittedSubDag>) -> Self {
        Self {
            receiver,
            next_expected_index: 1, // First commit has index 1 (consensus doesn't create commit with index 0)
            pending_commits: BTreeMap::new(),
            commit_index_callback: None,
            current_epoch: 0,
            last_global_exec_index: 0,
            executor_client: None,
            transition_barrier: None,
            global_exec_index_at_barrier: None,
            pending_transactions_queue: None,
        }
    }

    /// Set callback to notify commit index updates
    pub fn with_commit_index_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(u32) + Send + Sync + 'static,
    {
        self.commit_index_callback = Some(Arc::new(callback));
        self
    }

    /// Set epoch and last_global_exec_index for deterministic global_exec_index calculation
    pub fn with_epoch_info(mut self, epoch: u64, last_global_exec_index: u64) -> Self {
        self.current_epoch = epoch;
        self.last_global_exec_index = last_global_exec_index;
        self
    }

    /// Set executor client to send blocks to Go executor
    pub fn with_executor_client(mut self, executor_client: Arc<ExecutorClient>) -> Self {
        self.executor_client = Some(executor_client);
        self
    }

    /// Set transition barrier to prevent sending commits past barrier to Go Master
    /// This prevents duplicate global_exec_index between epochs
    /// Uses Arc<AtomicU32> so barrier can be set dynamically during epoch transition
    pub fn with_transition_barrier(mut self, barrier: Arc<AtomicU32>) -> Self {
        self.transition_barrier = Some(barrier);
        self
    }

    /// Set global exec index at barrier for commits past barrier
    /// Commits past barrier will be sent as one block with global_exec_index = barrier_global_exec_index + 1
    /// Uses Arc<AtomicU64> for thread-safe access
    pub fn with_global_exec_index_at_barrier(mut self, global_exec_index_at_barrier: Arc<AtomicU64>) -> Self {
        self.global_exec_index_at_barrier = Some(global_exec_index_at_barrier);
        self
    }

    /// Provide a queue to store transactions that need to be retried in the next epoch.
    pub fn with_pending_transactions_queue(
        mut self,
        pending_transactions_queue: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    ) -> Self {
        self.pending_transactions_queue = Some(pending_transactions_queue);
        self
    }

    /// Process commits in order
    pub async fn run(self) -> Result<()> {
        let mut receiver = self.receiver;
        let mut next_expected_index = self.next_expected_index;
        let mut pending_commits = self.pending_commits;
        let commit_index_callback = self.commit_index_callback;
        let current_epoch = self.current_epoch;
        let last_global_exec_index = self.last_global_exec_index;
        let executor_client = self.executor_client;
        let transition_barrier = self.transition_barrier;
        let global_exec_index_at_barrier = self.global_exec_index_at_barrier;
        let pending_transactions_queue = self.pending_transactions_queue;
        
        // Buffer for commits past barrier (will be sent as one block)
        // Track the last non-zero barrier we observed. This avoids a race where epoch transition
        // resets barrier back to 0 while the old CommitProcessor is still draining its receiver.
        // Without this, the old processor could start sending old-epoch commits again.
        let mut barrier_snapshot: u32 = 0;
        loop {
            match receiver.recv().await {
                Some(subdag) => {
                    let commit_index: u32 = subdag.commit_ref.index;
                    
                    // If this is the next expected commit, process it immediately
                    if commit_index == next_expected_index {
                        // CRITICAL FIX: Check barrier BEFORE calculating global_exec_index
                        // This prevents commits past barrier from being processed and sent
                        // Commits past barrier should be skipped entirely (not sent to Go Master)
                        let (is_past_barrier, barrier_value) = if let Some(barrier) = transition_barrier.as_ref() {
                            let barrier_val = barrier.load(Ordering::Relaxed);
                            if barrier_val > 0 {
                                barrier_snapshot = barrier_val;
                            }
                            let effective_barrier = if barrier_val > 0 { barrier_val } else { barrier_snapshot };
                            (effective_barrier > 0 && commit_index > effective_barrier, effective_barrier)
                        } else {
                            (false, 0)
                        };
                        
                        if is_past_barrier {
                            // Commit past barrier → skip entirely, don't calculate global_exec_index
                            // Re-queue transactions for next epoch
                            let total_txs = subdag.blocks.iter().map(|b| b.transactions().len()).sum::<usize>();
                            
                            let mut lost_tx_hashes = Vec::new();
                            let mut lost_tx_data: Vec<Vec<u8>> = Vec::new();
                            for block in &subdag.blocks {
                                for tx in block.transactions() {
                                    let tx_data = tx.data();
                                    let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
                                    lost_tx_hashes.push(tx_hash_hex);
                                    lost_tx_data.push(tx_data.to_vec());
                                }
                            }
                            
                            if !lost_tx_hashes.is_empty() {
                                warn!(
                                    "⚠️ [TX FLOW] Commit past barrier detected BEFORE processing (commit_index={} > barrier={}): {} transactions detected. Re-queuing for next epoch. tx_hashes={:?}",
                                    commit_index, barrier_value, total_txs, lost_tx_hashes
                                );
                            }
                            
                            if !lost_tx_data.is_empty() {
                                match pending_transactions_queue.as_ref() {
                                    Some(queue) => {
                                        let mut q = queue.lock().await;
                                        let before = q.len();
                                        q.extend(lost_tx_data);
                                        let after = q.len();
                                        info!(
                                            "📦 [TX FLOW] Re-queued {} transaction(s) from commit past barrier (before processing) for next epoch: queue_size {} -> {}",
                                            total_txs, before, after
                                        );
                                    }
                                    None => {
                                        warn!(
                                            "⚠️ [TX FLOW] Commit past barrier contained {} transaction(s) but pending_transactions_queue is not configured. Transactions must be retried by client.",
                                            total_txs
                                        );
                                    }
                                }
                            }
                            
                            warn!(
                                "⏭️ [FORK-SAFETY] Skipping commit past barrier BEFORE processing (commit_index={} > barrier={}): not calculating global_exec_index to prevent duplicate",
                                commit_index, barrier_value
                            );
                            
                            // Still advance next_expected_index to allow processing next commit
                            if let Some(ref callback) = commit_index_callback {
                                callback(commit_index);
                            }
                            next_expected_index += 1;
                            continue; // Skip this commit entirely
                        }
                        
                        // Calculate deterministic global_exec_index only if commit is not past barrier
                        // (We already checked barrier above and skipped if past barrier with continue)
                        let global_exec_index = calculate_global_exec_index(
                            current_epoch,
                            commit_index,
                            last_global_exec_index,
                        );
                        
                        // Process commit normally (not past barrier, already verified above)
                        Self::process_commit(&subdag, global_exec_index, current_epoch, executor_client.clone(), transition_barrier.clone(), global_exec_index_at_barrier.clone(), pending_transactions_queue.clone()).await?;
                        
                        // Notify commit index update (for epoch transition)
                        if let Some(ref callback) = commit_index_callback {
                            callback(commit_index);
                        }
                        
                        next_expected_index += 1;
                        
                        // Process any pending commits that are now in order
                        while let Some(pending) = pending_commits.remove(&next_expected_index) {
                            let pending_commit_index = next_expected_index;
                            
                            // CRITICAL FIX: Check barrier BEFORE calculating global_exec_index for pending commit
                            let (is_past_barrier, barrier_value) = if let Some(barrier) = transition_barrier.as_ref() {
                                let barrier_val = barrier.load(Ordering::Relaxed);
                                if barrier_val > 0 {
                                    barrier_snapshot = barrier_val;
                                }
                                let effective_barrier = if barrier_val > 0 { barrier_val } else { barrier_snapshot };
                                (effective_barrier > 0 && pending_commit_index > effective_barrier, effective_barrier)
                            } else {
                                (false, 0)
                            };
                            
                            if is_past_barrier {
                                let total_txs = pending.blocks.iter().map(|b| b.transactions().len()).sum::<usize>();

                                let mut lost_tx_hashes = Vec::new();
                                let mut lost_tx_data: Vec<Vec<u8>> = Vec::new();
                                for block in &pending.blocks {
                                    for tx in block.transactions() {
                                        let tx_data = tx.data();
                                        let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
                                        lost_tx_hashes.push(tx_hash_hex);
                                        lost_tx_data.push(tx_data.to_vec());
                                    }
                                }

                                if !lost_tx_hashes.is_empty() {
                                    warn!(
                                        "⚠️ [TX FLOW] Pending commit past barrier detected BEFORE processing (commit_index={} > barrier={}): {} transactions detected. Re-queuing for next epoch. tx_hashes={:?}",
                                        pending_commit_index, barrier_value, total_txs, lost_tx_hashes
                                    );
                                }
                                if !lost_tx_data.is_empty() {
                                    match pending_transactions_queue.as_ref() {
                                        Some(queue) => {
                                            let mut q = queue.lock().await;
                                            let before = q.len();
                                            q.extend(lost_tx_data);
                                            let after = q.len();
                                            info!(
                                                "📦 [TX FLOW] Re-queued {} transaction(s) from pending commit past barrier (before processing) for next epoch: queue_size {} -> {}",
                                                total_txs, before, after
                                            );
                                        }
                                        None => {
                                            warn!(
                                                "⚠️ [TX FLOW] Pending commit past barrier contained {} transaction(s) but pending_transactions_queue is not configured. Transactions must be retried by client.",
                                                total_txs
                                            );
                                        }
                                    }
                                }

                                warn!(
                                    "⏭️ [FORK-SAFETY] Skipping pending commit past barrier BEFORE processing (commit_index={} > barrier={}): not calculating global_exec_index to prevent duplicate",
                                    pending_commit_index, barrier_value
                                );
                                
                                // Still advance next_expected_index
                                if let Some(ref callback) = commit_index_callback {
                                    callback(pending_commit_index);
                                }
                                next_expected_index += 1;
                                continue; // Skip this pending commit entirely
                            }
                            
                            // Calculate deterministic global_exec_index only if commit is not past barrier
                            let global_exec_index = calculate_global_exec_index(
                                current_epoch,
                                pending_commit_index,
                                last_global_exec_index,
                            );
                            
                            // Process commit normally (not past barrier, already verified above)
                            Self::process_commit(&pending, global_exec_index, current_epoch, executor_client.clone(), transition_barrier.clone(), global_exec_index_at_barrier.clone(), pending_transactions_queue.clone()).await?;
                            
                            // Notify commit index update
                            if let Some(ref callback) = commit_index_callback {
                                callback(pending_commit_index);
                            }
                            
                            next_expected_index += 1;
                        }
                    } else if commit_index > next_expected_index {
                        // Out of order - store for later
                        warn!(
                            "Received out-of-order commit: index={}, expected={}, storing for later",
                            commit_index, next_expected_index
                        );
                        pending_commits.insert(commit_index, subdag);
                    } else {
                        // Duplicate or old commit
                        warn!(
                            "Received commit with index {} which is less than expected {}",
                            commit_index, next_expected_index
                        );
                    }
                }
                None => {
                    // Expected during epoch transition / authority restart (commit consumer is dropped).
                    tracing::debug!("Commit receiver closed");
                    break;
                }
            }
        }
        Ok(())
    }

    async fn process_commit(
        subdag: &CommittedSubDag,
        global_exec_index: u64,
        epoch: u64,
        executor_client: Option<Arc<ExecutorClient>>,
        transition_barrier: Option<Arc<AtomicU32>>,
        _global_exec_index_at_barrier: Option<Arc<AtomicU64>>,
        pending_transactions_queue: Option<Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>>,
    ) -> Result<()> {
        let commit_index = subdag.commit_ref.index;
        
        // CRITICAL RACE CONDITION FIX: Double-check barrier RIGHT BEFORE sending
        // This prevents race condition where commit is processed before barrier is set,
        // but barrier gets set before commit is sent → commit should be skipped
        // Example timeline:
        // 1. Commit #1212 processed (barrier not set yet)
        // 2. Barrier set = 1209 (commit #1212 > barrier → past barrier)
        // 3. Commit #1212 about to be sent → should be skipped!
        let mut barrier_snapshot: u32 = 0;
        let (is_past_barrier_now, barrier_value_now) = if let Some(barrier) = transition_barrier.as_ref() {
            let barrier_val = barrier.load(Ordering::Relaxed);
            if barrier_val > 0 {
                barrier_snapshot = barrier_val;
            }
            let effective_barrier = if barrier_val > 0 { barrier_val } else { barrier_snapshot };
            (effective_barrier > 0 && commit_index > effective_barrier, effective_barrier)
        } else {
            (false, 0)
        };
        
        if is_past_barrier_now {
            // Barrier was set between processing and sending → skip send and re-queue transactions
            let total_txs = subdag.blocks.iter().map(|b| b.transactions().len()).sum::<usize>();
            
            let mut lost_tx_hashes = Vec::new();
            let mut lost_tx_data: Vec<Vec<u8>> = Vec::new();
            for block in &subdag.blocks {
                for tx in block.transactions() {
                    let tx_data = tx.data();
                    let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
                    lost_tx_hashes.push(tx_hash_hex);
                    lost_tx_data.push(tx_data.to_vec());
                }
            }
            
            if !lost_tx_hashes.is_empty() {
                warn!(
                    "⚠️ [TX FLOW] RACE CONDITION: Commit past barrier detected RIGHT BEFORE SEND (commit_index={} > barrier={}): {} transactions detected. Re-queuing for next epoch. tx_hashes={:?}",
                    commit_index, barrier_value_now, total_txs, lost_tx_hashes
                );
            }
            
            // Re-queue transactions if queue is available
            if !lost_tx_data.is_empty() {
                match pending_transactions_queue.as_ref() {
                    Some(queue) => {
                        let mut q = queue.lock().await;
                        let before = q.len();
                        q.extend(lost_tx_data);
                        let after = q.len();
                        info!(
                            "📦 [TX FLOW] Re-queued {} transaction(s) from race condition commit past barrier for next epoch: queue_size {} -> {}",
                            total_txs, before, after
                        );
                    }
                    None => {
                        warn!(
                            "⚠️ [TX FLOW] RACE CONDITION: Commit past barrier contained {} transaction(s) but pending_transactions_queue is not configured. Transactions must be retried by client.",
                            total_txs
                        );
                    }
                }
            }
            
            warn!(
                "⏭️ [FORK-SAFETY] RACE CONDITION: Skipping commit past barrier RIGHT BEFORE SEND (commit_index={} > barrier={}): not sending to Go executor to prevent duplicate global_exec_index (global_exec_index={})",
                commit_index, barrier_value_now, global_exec_index
            );
            
            // Return early - don't send commit
            return Ok(());
        }
        let mut total_transactions = 0;
        let mut transaction_hashes = Vec::new();
        let mut block_details = Vec::new();
        
        // Process blocks in commit order
        for (block_idx, block) in subdag.blocks.iter().enumerate() {
            let transactions = block.transactions();
            let block_tx_count = transactions.len();
            total_transactions += block_tx_count;
            
            // Calculate official transaction hashes (Keccak256 from TransactionHashData)
            let mut block_tx_hashes = Vec::new();
            for tx in transactions {
                let tx_data = tx.data();
                let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
                transaction_hashes.push(tx_hash_hex.clone());
                block_tx_hashes.push(tx_hash_hex);
            }
            
            // Store block details
            block_details.push(format!(
                "block[{}]={:?} ({}tx)",
                block_idx,
                block.reference(),
                block_tx_count
            ));
        }
        
        if total_transactions > 0 {
            info!(
                "🔷 [Global Index: {}] Executing commit #{} (epoch={}): leader={:?}, {} blocks, {} total transactions, tx_hashes=[{}]",
                global_exec_index,  // Deterministic Checkpoint Sequence Number (Global Index) - hiển thị đầu tiên để dễ theo dõi
                commit_index,
                epoch,
                subdag.leader,
                subdag.blocks.len(),
                total_transactions,
                transaction_hashes.iter().take(10).map(|h| h.as_str()).collect::<Vec<_>>().join(", ")
            );
            info!(
                "   📦 Blocks in commit #{}: {}",
                commit_index,
                block_details.join(", ")
            );
            
            // Send committed subdag to Go executor if enabled
            // CRITICAL FORK-SAFETY: Include global_exec_index to ensure deterministic execution order
            // Note: Commits past barrier are already buffered and sent as one block, so this function
            // only processes commits at or before barrier
            let send_epoch = epoch;
            let send_global_exec_index = global_exec_index;
            
            if let Some(ref client) = executor_client {
                info!("📤 [TX FLOW] Sending committed subdag to Go executor: global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_tx={}", 
                    send_global_exec_index, commit_index, send_epoch, subdag.blocks.len(), total_transactions);
                if let Err(e) = client.send_committed_subdag(subdag, send_epoch, send_global_exec_index).await {
                    warn!("⚠️  [TX FLOW] Failed to send committed subdag to executor: {}", e);
                    // Don't fail commit if executor is unavailable
                } else {
                    info!("✅ [TX FLOW] Successfully sent committed subdag to Go executor: global_exec_index={}, commit_index={}, epoch={}, blocks={}", 
                        send_global_exec_index, commit_index, send_epoch, subdag.blocks.len());
                }
            } else {
                info!("ℹ️  [TX FLOW] Executor client not enabled, skipping send to Go executor");
            }
        } else {
            info!(
                "🔷 [Global Index: {}] Executing commit #{} (epoch={}): leader={:?}, {} blocks, 0 transactions",
                global_exec_index,  // Deterministic Checkpoint Sequence Number (Global Index) - hiển thị đầu tiên để dễ theo dõi
                commit_index,
                epoch,
                subdag.leader,
                subdag.blocks.len()
            );
            if subdag.blocks.len() > 1 {
                info!(
                    "   📦 Blocks in commit #{}: {}",
                    commit_index,
                    block_details.join(", ")
                );
            }
            
            // Send committed subdag to Go executor if enabled (even for empty commits)
            // CRITICAL FORK-SAFETY: Include global_exec_index to ensure deterministic execution order
            // Note: Commits past barrier are already buffered and sent as one block, so this function
            // only processes commits at or before barrier
            let send_epoch = epoch;
            let send_global_exec_index = global_exec_index;
            
            if let Some(ref client) = executor_client {
                if let Err(e) = client.send_committed_subdag(subdag, send_epoch, send_global_exec_index).await {
                    warn!("⚠️  Failed to send committed subdag to executor: {}", e);
                    // Don't fail commit if executor is unavailable
                }
            }
        }
        
        Ok(())
    }
}

