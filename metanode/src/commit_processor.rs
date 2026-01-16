// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI};
use mysten_metrics::monitored_mpsc::UnboundedReceiver;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration; // [Added] Import Duration
use tokio::time::sleep;  // [Added] Import sleep for retry mechanism
use tracing::{info, warn, error};
use hex;

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
    /// Optional callback to update global execution index after successful commit
    global_exec_index_callback: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Current epoch (for deterministic global_exec_index calculation)
    current_epoch: u64,
    /// Callback to get current last global execution index
    get_last_global_exec_index: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    /// Shared last global exec index for direct updates
    shared_last_global_exec_index: Option<Arc<tokio::sync::Mutex<u64>>>,
    /// Optional executor client to send blocks to Go executor
    executor_client: Option<Arc<ExecutorClient>>,
    /// Flag indicating if epoch transition is in progress
    /// When true, we're transitioning to a new epoch
    is_transitioning: Option<Arc<AtomicBool>>,
    /// Queue for transactions that must be retried in the next epoch
    pending_transactions_queue: Option<Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>>,
    /// Optional callback to handle EndOfEpoch system transactions
    /// Called immediately when an EndOfEpoch system transaction is detected in a committed sub-dag
    /// Uses commit finalization approach (like Sui) - no buffer needed as commits are processed sequentially
    epoch_transition_callback: Option<Arc<dyn Fn(u64, u64, u32) -> Result<()> + Send + Sync>>,
}

impl CommitProcessor {
    pub fn new(receiver: UnboundedReceiver<CommittedSubDag>) -> Self {
        Self {
            receiver,
            next_expected_index: 1, // First commit has index 1 (consensus doesn't create commit with index 0)
            pending_commits: BTreeMap::new(),
            commit_index_callback: None,
            global_exec_index_callback: None,
            get_last_global_exec_index: None,
            shared_last_global_exec_index: None,
            current_epoch: 0,
            executor_client: None,
            is_transitioning: None,
            pending_transactions_queue: None,
            epoch_transition_callback: None,
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

    /// Set callback to update global execution index after successful commit
    pub fn with_global_exec_index_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(u64) + Send + Sync + 'static,
    {
        self.global_exec_index_callback = Some(Arc::new(callback));
        self
    }

    /// Set callback to get current last global execution index
    pub fn with_get_last_global_exec_index<F>(self, _callback: F) -> Self
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        // Currently not used, but kept for future extensibility
        // CRITICAL: We now read directly from shared_last_global_exec_index instead of callback
        self
    }

    /// Set shared last global exec index for direct updates
    pub fn with_shared_last_global_exec_index(mut self, shared_index: Arc<tokio::sync::Mutex<u64>>) -> Self {
        self.shared_last_global_exec_index = Some(shared_index);
        self
    }

    /// Set epoch and last_global_exec_index for deterministic global_exec_index calculation
    pub fn with_epoch_info(mut self, epoch: u64, _last_global_exec_index: u64) -> Self {
        self.current_epoch = epoch;
        // last_global_exec_index is now obtained from shared_last_global_exec_index, no need to store locally
        self
    }

    /// Set executor client to send blocks to Go executor
    pub fn with_executor_client(mut self, executor_client: Arc<ExecutorClient>) -> Self {
        self.executor_client = Some(executor_client);
        self
    }

    /// Set is_transitioning flag to track epoch transition state
    pub fn with_is_transitioning(mut self, is_transitioning: Arc<AtomicBool>) -> Self {
        self.is_transitioning = Some(is_transitioning);
        self
    }

    /// Provide a queue to store transactions that must be retried in the next epoch.
    pub fn with_pending_transactions_queue(
        mut self,
        pending_transactions_queue: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    ) -> Self {
        self.pending_transactions_queue = Some(pending_transactions_queue);
        self
    }

    /// Set callback to handle EndOfEpoch system transactions
    pub fn with_epoch_transition_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(u64, u64, u32) -> Result<()> + Send + Sync + 'static,
    {
        self.epoch_transition_callback = Some(Arc::new(callback));
        self
    }

    /// Process commits in order
    pub async fn run(self) -> Result<()> {
        let mut receiver = self.receiver;
        let mut next_expected_index = self.next_expected_index;
        let mut pending_commits = self.pending_commits;
        let commit_index_callback = self.commit_index_callback;
        let current_epoch = self.current_epoch;
        // CRITICAL: We now read directly from shared_last_global_exec_index in the loop
        // No need to read it here as it will be updated after each commit
        let executor_client = self.executor_client;
        let pending_transactions_queue = self.pending_transactions_queue;
        let epoch_transition_callback = self.epoch_transition_callback;
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            // Get initial last_global_exec_index from shared index for logging
            let initial_last_global_exec_index = if let Some(ref shared_index) = self.shared_last_global_exec_index {
                let index_guard = shared_index.lock().await;
                *index_guard
            } else {
                0
            };
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:124","message":"COMMIT PROCESSOR STARTED","data":{{"current_epoch":{},"last_global_exec_index":{},"next_expected_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    current_epoch, initial_last_global_exec_index, next_expected_index);
            }
        }
        // #endregion
        // Get initial last_global_exec_index from shared index for logging
        let initial_last_global_exec_index = if let Some(ref shared_index) = self.shared_last_global_exec_index {
            let index_guard = shared_index.lock().await;
            *index_guard
        } else {
            0
        };
        info!("🚀 [COMMIT PROCESSOR] Started processing commits for epoch {} (last_global_exec_index={}, next_expected_index={})",
            current_epoch, initial_last_global_exec_index, next_expected_index);
        
        let mut last_heartbeat_commit = 0u32;
        let mut last_heartbeat_time = std::time::Instant::now();
        const HEARTBEAT_INTERVAL: u32 = 1000; 
        const HEARTBEAT_TIMEOUT_SECS: u64 = 300; 
        
        info!("📡 [COMMIT PROCESSOR] Waiting for commits from consensus...");
        
        loop {
            match receiver.recv().await {
                Some(subdag) => {
                    let commit_index: u32 = subdag.commit_ref.index;
                    info!("📥 [COMMIT PROCESSOR] Received committed subdag: commit_index={}, leader={:?}, blocks={}",
                        commit_index, subdag.leader, subdag.blocks.len());
                    
                    info!("📊 [COMMIT CONDITION] Checking commit_index={}, next_expected_index={}", commit_index, next_expected_index);
                    if commit_index == next_expected_index {
                        // CRITICAL FIX: Read directly from shared_last_global_exec_index instead of callback
                        // Callback may return 0 if called from async context, causing incorrect global_exec_index calculation
                        let current_last_global_exec_index = if let Some(ref shared_index) = self.shared_last_global_exec_index {
                            let index_guard = shared_index.lock().await;
                            *index_guard
                        } else {
                            // Fallback: try callback if shared index not available
                            if let Some(ref callback) = self.get_last_global_exec_index {
                                callback()
                            } else {
                                0 // final fallback
                            }
                        };

                        let global_exec_index = calculate_global_exec_index(
                            current_epoch,
                            commit_index,
                            current_last_global_exec_index,
                        );

                        info!("📊 [GLOBAL_EXEC_INDEX] Calculated: global_exec_index={}, epoch={}, commit_index={}, current_last_global_exec_index={}",
                            global_exec_index, current_epoch, commit_index, current_last_global_exec_index);

                        // Note: shared index will be updated in process_commit after successful send
                        
                        let total_txs_in_commit = subdag.blocks.iter().map(|b| b.transactions().len()).sum::<usize>();

                        // Check for EndOfEpoch system transactions
                        let _has_system_tx = subdag.extract_end_of_epoch_transaction().is_some();
                        if let Some((_block_ref, system_tx)) = subdag.extract_end_of_epoch_transaction() {
                            if let Some((new_epoch, new_epoch_timestamp_ms, _commit_index_from_tx)) = system_tx.as_end_of_epoch() {
                                info!(
                                    "🎯 [SYSTEM TX] EndOfEpoch transaction detected in commit {}: epoch {} -> {}, total_txs_in_commit={}",
                                    commit_index, current_epoch, new_epoch, total_txs_in_commit
                                );
                                
                                if let Some(ref callback) = epoch_transition_callback {
                                    info!(
                                        "🚀 [EPOCH TRANSITION] Triggering epoch transition immediately: commit_index={}, new_epoch={}",
                                        commit_index, new_epoch
                                    );
                                    
                                    if let Err(e) = callback(new_epoch, new_epoch_timestamp_ms, commit_index) {
                                        warn!("❌ Failed to trigger epoch transition from system transaction: {}", e);
                                    }
                                }
                            }
                        }
                        
                        // Process commit normally
                        info!("📊 [COMMIT_PROCESSOR] About to call process_commit with shared_index is_some={}", self.shared_last_global_exec_index.is_some());
                        Self::process_commit(&subdag, global_exec_index, current_epoch, executor_client.clone(), pending_transactions_queue.clone(), self.shared_last_global_exec_index.clone()).await?;
                        info!("📊 [COMMIT_PROCESSOR] process_commit returned Ok");

                        // Update global execution index after successful commit
                        // This ensures next commit gets the correct sequential block number
                        // NOTE: This callback is for compatibility, but actual update happens in process_commit
                        if let Some(ref callback) = self.global_exec_index_callback {
                            callback(global_exec_index);
                        }

                        // NOTE: Shared index is now updated in process_commit for both empty and non-empty commits
                        // No need to update here again to avoid duplicate updates

                        if let Some(ref callback) = commit_index_callback {
                            callback(commit_index);
                        }
                        
                        // Heartbeat logic
                        if commit_index >= last_heartbeat_commit + HEARTBEAT_INTERVAL {
                            let elapsed = last_heartbeat_time.elapsed().as_secs();
                            info!("💓 [COMMIT PROCESSOR HEARTBEAT] Processed {} commits (last {} commits in {}s)", 
                                commit_index, HEARTBEAT_INTERVAL, elapsed);
                            last_heartbeat_commit = commit_index;
                            last_heartbeat_time = std::time::Instant::now();
                        }
                        
                        let time_since_last_heartbeat = last_heartbeat_time.elapsed().as_secs();
                        if time_since_last_heartbeat > HEARTBEAT_TIMEOUT_SECS && commit_index == last_heartbeat_commit {
                            warn!("⚠️  [COMMIT PROCESSOR] Possible stuck detected: No progress for {}s (last commit: {})", 
                                time_since_last_heartbeat, commit_index);
                        }
                        
                        next_expected_index += 1;
                        
                        while let Some(pending) = pending_commits.remove(&next_expected_index) {
                            let pending_commit_index = next_expected_index;

                            // CRITICAL FIX: Read directly from shared_last_global_exec_index instead of callback
                            // Callback may return 0 if called from async context, causing incorrect global_exec_index calculation
                            let current_last_global_exec_index = if let Some(ref shared_index) = self.shared_last_global_exec_index {
                                let index_guard = shared_index.lock().await;
                                *index_guard
                            } else {
                                // Fallback: try callback if shared index not available
                                if let Some(ref callback) = self.get_last_global_exec_index {
                                    callback()
                                } else {
                                    0 // final fallback
                                }
                            };

                            let global_exec_index = calculate_global_exec_index(
                                current_epoch,
                                pending_commit_index,
                                current_last_global_exec_index,
                            );
                            
                            Self::process_commit(&pending, global_exec_index, current_epoch, executor_client.clone(), pending_transactions_queue.clone(), self.shared_last_global_exec_index.clone()).await?;
                            
                            if let Some(ref callback) = commit_index_callback {
                                callback(pending_commit_index);
                            }
                            
                            next_expected_index += 1;
                        }
                    } else if commit_index > next_expected_index {
                        warn!(
                            "Received out-of-order commit: index={}, expected={}, storing for later",
                            commit_index, next_expected_index
                        );
                        pending_commits.insert(commit_index, subdag);
                    } else {
                        warn!(
                            "Received commit with index {} which is less than expected {}",
                            commit_index, next_expected_index
                        );
                    }
                }
                None => {
                    warn!("⚠️  [COMMIT PROCESSOR] Commit receiver closed (commit processor will exit).");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Queue all user transactions from a failed commit for processing in the next epoch
    async fn queue_commit_transactions_for_next_epoch(
        subdag: &CommittedSubDag,
        pending_transactions_queue: &Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
        commit_index: u32,
        global_exec_index: u64,
        epoch: u64,
    ) {
        let has_end_of_epoch = subdag.extract_end_of_epoch_transaction().is_some();

        let mut queued_count = 0;
        let mut skipped_count = 0;

        for (block_idx, block) in subdag.blocks.iter().enumerate() {
            for (tx_idx, tx) in block.transactions().iter().enumerate() {
                let tx_data = tx.data();

                // Skip EndOfEpoch system transactions - they are epoch-specific
                if has_end_of_epoch && Self::is_end_of_epoch_transaction(tx_data) {
                    // NOTE: If we are here, it means the retry loop in process_commit failed or gave up.
                    // Dropping the EndOfEpoch here implies we are forced to move on without executing it.
                    info!("ℹ️  [TX FLOW] Skipping EndOfEpoch system transaction in failed commit {} (epoch-specific, cannot be queued for next epoch)", commit_index);
                    skipped_count += 1;
                    continue;
                }

                // Queue the transaction for next epoch
                let mut queue = pending_transactions_queue.lock().await;
                queue.push(tx_data.to_vec());
                queued_count += 1;

                let tx_hash = crate::tx_hash::calculate_transaction_hash(tx_data);
                let tx_hash_hex = hex::encode(&tx_hash[..8]);

                info!("📦 [TX FLOW] Queued failed transaction from commit {} block {} tx {}: hash={} (size={})",
                    commit_index, block_idx, tx_idx, tx_hash_hex, tx_data.len());
            }
        }

        if queued_count > 0 {
            info!("✅ [TX FLOW] Queued {} transactions from failed commit {} (global_exec_index={}, epoch={}) for next epoch",
                queued_count, commit_index, global_exec_index, epoch);
        }

        if skipped_count > 0 {
            info!("ℹ️  [TX FLOW] Skipped {} system transactions from failed commit {} (not suitable for next epoch)",
                skipped_count, commit_index);
        }
    }

    fn is_end_of_epoch_transaction(tx_data: &[u8]) -> bool {
        if tx_data.len() < 10 {
            return false;
        }
        let data_str = String::from_utf8_lossy(tx_data);
        data_str.contains("EndOfEpoch") || data_str.contains("epoch") && data_str.contains("transition")
    }

    async fn process_commit(
        subdag: &CommittedSubDag,
        global_exec_index: u64,
        epoch: u64,
        executor_client: Option<Arc<ExecutorClient>>,
        pending_transactions_queue: Option<Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>>,
        shared_last_global_exec_index: Option<Arc<tokio::sync::Mutex<u64>>>,
    ) -> Result<()> {
        let commit_index = subdag.commit_ref.index;
        let mut total_transactions = 0;
        let mut transaction_hashes = Vec::new();
        let mut block_details = Vec::new();
        
        for (block_idx, block) in subdag.blocks.iter().enumerate() {
            let transactions = block.transactions();
            total_transactions += transactions.len();
            
            for tx in transactions {
                let tx_data = tx.data();
                let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
                transaction_hashes.push(tx_hash_hex);
            }
            
            block_details.push(format!("block[{}]={:?} ({}tx)", block_idx, block.reference(), transactions.len()));
        }
        
        let has_system_tx = subdag.extract_end_of_epoch_transaction().is_some();

        if total_transactions > 0 || has_system_tx {
            // Log detailed transaction count per block
            let block_tx_counts: Vec<usize> = subdag.blocks.iter()
                .map(|b| b.transactions().len())
                .collect();
            
            info!(
                "🔷 [Global Index: {}] Executing commit #{} (epoch={}): {} blocks, {} txs, has_system_tx={}",
                global_exec_index, commit_index, epoch, subdag.blocks.len(), total_transactions, has_system_tx
            );
            
            // Log transaction count per block
            for (block_idx, tx_count) in block_tx_counts.iter().enumerate() {
                if *tx_count > 0 {
                    info!("  📦 Block[{}]: {} transactions", block_idx, tx_count);
                }
            }

            if let Some(ref client) = executor_client {
                // FIXED: Wrapped in a loop to retry system transactions
                let mut retry_count = 0;
                loop {
                    match client.send_committed_subdag(subdag, epoch, global_exec_index).await {
                        Ok(_) => {
                            // Store block in global cache for full node sync
                            info!("📦 [BLOCK CACHE] Attempting to store block {} in cache", global_exec_index);
                            if let Err(e) = crate::block_cache::store_block(global_exec_index, subdag).await {
                                warn!("⚠️ [BLOCK CACHE] Failed to store block {} in cache: {}", global_exec_index, e);
                            } else {
                                info!("✅ [BLOCK CACHE] Successfully stored block {} in cache", global_exec_index);
                            }

                            info!("✅ [TX FLOW] Successfully sent committed subdag: global_exec_index={}, commit_index={}",
                                global_exec_index, commit_index);

                            // CRITICAL: Update shared last global exec index SYNCHRONOUSLY after successful send
                            // This ensures next commit gets the correct sequential block number
                            // We must update immediately, not in a spawned task, to prevent race conditions
                            if let Some(shared_index) = shared_last_global_exec_index.clone() {
                                let mut index_guard = shared_index.lock().await;
                                *index_guard = global_exec_index;
                                info!("📊 [GLOBAL_EXEC_INDEX] Updated shared last_global_exec_index to {} after successful send (synchronous)", global_exec_index);
                            }

                            break;
                        },
                        Err(e) => {
                            // Case 1: Duplicate index - Critical Fork Safety check
                            if e.to_string().contains("Duplicate global_exec_index") {
                                error!("🚨 [FORK-SAFETY] Duplicate global_exec_index={} detected! Skipping commit {} to prevent fork. Error: {}", 
                                    global_exec_index, commit_index, e);
                                // For duplicates, we break loop and do NOT queue transactions because they are likely already executed
                                break;
                            }
                            
                            // Case 2: System Transaction (EndOfEpoch) failed
                            // We MUST NOT skip this. We retry indefinitely (or until success) because dropping it prevents epoch change.
                            if has_system_tx {
                                retry_count += 1;
                                error!("🚨 [CRITICAL] Failed to send commit {} containing EndOfEpoch transaction (Attempt {}). Retrying in 1s... Error: {}", 
                                    commit_index, retry_count, e);
                                
                                sleep(Duration::from_secs(1)).await;
                                continue; // Retry the loop
                            }

                            // Case 3: Regular transaction failure (Network issue / Executor crash)
                            warn!("⚠️  [TX FLOW] Failed to send committed subdag: {}", e);
                            if let Some(ref queue) = pending_transactions_queue {
                                Self::queue_commit_transactions_for_next_epoch(subdag, queue, commit_index, global_exec_index, epoch).await;
                            } else {
                                warn!("⚠️  [TX FLOW] No pending_transactions_queue - transactions may be lost!");
                            }
                            break; // Exit loop, having queued what we could
                        }
                    }
                }
            } else {
                info!("ℹ️  [TX FLOW] Executor client not enabled, skipping send");
            }
        } else {
            // Empty commit handling (heartbeat/tick)
             if let Some(ref client) = executor_client {
                match client.send_committed_subdag(subdag, epoch, global_exec_index).await {
                    Ok(_) => {
                        // Store block in global cache for full node sync (even empty commits)
                        info!("📦 [BLOCK CACHE] Attempting to store empty block {} in cache", global_exec_index);
                        if let Err(e) = crate::block_cache::store_block(global_exec_index, subdag).await {
                            warn!("⚠️ [BLOCK CACHE] Failed to store empty block {} in cache: {}", global_exec_index, e);
                        } else {
                            info!("✅ [BLOCK CACHE] Successfully stored empty block {} in cache", global_exec_index);
                        }

                        info!("✅ [TX FLOW] Successfully sent empty commit: global_exec_index={}, commit_index={}",
                            global_exec_index, commit_index);

                        // CRITICAL: Update shared index for empty commits SYNCHRONOUSLY too
                        // This ensures sequential global_exec_index even for empty commits
                        // We must update immediately to prevent race conditions
                        if let Some(shared_index) = shared_last_global_exec_index.clone() {
                            let mut index_guard = shared_index.lock().await;
                            *index_guard = global_exec_index;
                            info!("📊 [GLOBAL_EXEC_INDEX] Updated shared last_global_exec_index to {} for empty commit (synchronous)", global_exec_index);
                        }
                    },
                    Err(e) => {
                        if e.to_string().contains("Duplicate global_exec_index") {
                            warn!("🚨 [FORK-SAFETY] Duplicate global_exec_index={} detected for empty commit. Skipping.", global_exec_index);
                        } else {
                            warn!("⚠️  Failed to send empty subdag: {}", e);
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
}