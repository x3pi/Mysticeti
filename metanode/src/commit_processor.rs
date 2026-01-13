// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI};
use mysten_metrics::monitored_mpsc::UnboundedReceiver;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
    /// Current epoch (for deterministic global_exec_index calculation)
    current_epoch: u64,
    /// Last global execution index from previous epoch (for deterministic calculation)
    last_global_exec_index: u64,
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
            current_epoch: 0,
            last_global_exec_index: 0,
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

    /// Set is_transitioning flag to track epoch transition state
    pub fn with_is_transitioning(mut self, is_transitioning: Arc<AtomicBool>) -> Self {
        self.is_transitioning = Some(is_transitioning);
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

    /// Set callback to handle EndOfEpoch system transactions
    /// The callback receives (new_epoch, new_epoch_timestamp_ms, commit_index)
    /// Uses commit finalization approach - transition is triggered immediately when system transaction is detected
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
        let last_global_exec_index = self.last_global_exec_index;
        let executor_client = self.executor_client;
        let pending_transactions_queue = self.pending_transactions_queue;
        let epoch_transition_callback = self.epoch_transition_callback;
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:124","message":"COMMIT PROCESSOR STARTED","data":{{"current_epoch":{},"last_global_exec_index":{},"next_expected_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    current_epoch, last_global_exec_index, next_expected_index);
            }
        }
        // #endregion
        info!("🚀 [COMMIT PROCESSOR] Started processing commits for epoch {} (last_global_exec_index={}, next_expected_index={})",
            current_epoch, last_global_exec_index, next_expected_index);
        
        // Heartbeat monitoring: Log every 1000 commits to detect if processor is stuck
        let mut last_heartbeat_commit = 0u32;
        let mut last_heartbeat_time = std::time::Instant::now();
        const HEARTBEAT_INTERVAL: u32 = 1000; // Log every 1000 commits
        const HEARTBEAT_TIMEOUT_SECS: u64 = 300; // 5 minutes timeout
        
        info!("📡 [COMMIT PROCESSOR] Waiting for commits from consensus...");
        
        loop {
            match receiver.recv().await {
                Some(subdag) => {
                    let commit_index: u32 = subdag.commit_ref.index;
                    info!("📥 [COMMIT PROCESSOR] Received committed subdag: commit_index={}, leader={:?}, blocks={}",
                        commit_index, subdag.leader, subdag.blocks.len());
                    
                    // If this is the next expected commit, process it immediately
                    if commit_index == next_expected_index {
                        // #region agent log
                        {
                            use std::io::Write;
                            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:147","message":"BEFORE calculate global_exec_index","data":{{"current_epoch":{},"commit_index":{},"last_global_exec_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                                    ts.as_secs(), ts.as_nanos() % 1000000,
                                    ts.as_millis(),
                                    current_epoch, commit_index, last_global_exec_index);
                            }
                        }
                        // #endregion
                        let global_exec_index = calculate_global_exec_index(
                            current_epoch,
                            commit_index,
                            last_global_exec_index,
                        );
                        
                        // CRITICAL: Log global_exec_index calculation for duplicate detection
                        info!("📊 [GLOBAL_EXEC_INDEX] Calculated: global_exec_index={}, epoch={}, commit_index={}, last_global_exec_index={}", 
                            global_exec_index, current_epoch, commit_index, last_global_exec_index);
                        
                        // #region agent log
                        {
                            use std::io::Write;
                            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:151","message":"AFTER calculate global_exec_index","data":{{"current_epoch":{},"commit_index":{},"last_global_exec_index":{},"calculated_global_exec_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                                    ts.as_secs(), ts.as_nanos() % 1000000,
                                    ts.as_millis(),
                                    current_epoch, commit_index, last_global_exec_index, global_exec_index);
                            }
                        }
                        // #endregion
                        
                        // Check for EndOfEpoch system transactions BEFORE processing commit
                        // FORK-SAFETY: Use commit finalization approach (like Sui) - trigger transition immediately
                        // Sequential processing ensures all nodes process commits in the same order,
                        // so no buffer is needed. All nodes will trigger transition at the same commit_index.
                        let total_txs_in_commit = subdag.blocks.iter().map(|b| b.transactions().len()).sum::<usize>();
                        
                        if let Some((_block_ref, system_tx)) = subdag.extract_end_of_epoch_transaction() {
                            if let Some((new_epoch, new_epoch_timestamp_ms, _commit_index_from_tx)) = system_tx.as_end_of_epoch() {
                                info!(
                                    "🎯 [SYSTEM TX] EndOfEpoch transaction detected in commit {}: epoch {} -> {}, total_txs_in_commit={} (including EndOfEpoch system tx)",
                                    commit_index, current_epoch, new_epoch, total_txs_in_commit
                                );
                                
                                // COMMIT FINALIZATION APPROACH: Trigger transition immediately
                                // Sequential processing ensures all nodes see the same commit at the same commit_index
                                // No buffer needed - consensus guarantees commit order
                                if let Some(ref callback) = epoch_transition_callback {
                                    info!(
                                        "🚀 [EPOCH TRANSITION] Triggering epoch transition immediately (commit finalization): commit_index={}, new_epoch={}, total_txs_in_commit={}",
                                        commit_index, new_epoch, total_txs_in_commit
                                    );
                                    
                                    if let Err(e) = callback(new_epoch, new_epoch_timestamp_ms, commit_index) {
                                        warn!("❌ Failed to trigger epoch transition from system transaction: {}", e);
                                    } else {
                                        info!(
                                            "✅ [EPOCH TRANSITION] Successfully triggered epoch transition: commit_index={}, new_epoch={}, total_txs_in_commit={}",
                                            commit_index, new_epoch, total_txs_in_commit
                                        );
                                    }
                                } else {
                                    warn!(
                                        "⚠️ [EPOCH TRANSITION] EndOfEpoch transaction detected but no callback configured: commit_index={}, new_epoch={}",
                                        commit_index, new_epoch
                                    );
                                }
                            }
                        }
                        
                        // Process commit normally
                        // IMPORTANT: Commit containing EndOfEpoch transaction will be processed normally
                        // All transactions in this commit (including regular transactions) will be sent to executor
                        // Note: extract_end_of_epoch_transaction() does NOT remove the transaction from the commit,
                        // so all transactions (including EndOfEpoch system transaction) will be sent to executor
                        info!(
                            "📦 [TX FLOW] Processing commit {} with {} total transactions (will be sent to executor including all transactions)",
                            commit_index, total_txs_in_commit
                        );
                        Self::process_commit(&subdag, global_exec_index, current_epoch, executor_client.clone(), pending_transactions_queue.clone()).await?;
                        
                        // Notify commit index update (for epoch transition)
                        if let Some(ref callback) = commit_index_callback {
                            callback(commit_index);
                        }
                        
                        // Heartbeat monitoring: Log progress and detect stuck
                        if commit_index >= last_heartbeat_commit + HEARTBEAT_INTERVAL {
                            let elapsed = last_heartbeat_time.elapsed().as_secs();
                            info!("💓 [COMMIT PROCESSOR HEARTBEAT] Processed {} commits (last {} commits in {}s, avg {:.2} commits/s)", 
                                commit_index, HEARTBEAT_INTERVAL, elapsed, 
                                HEARTBEAT_INTERVAL as f64 / elapsed.max(1) as f64);
                            last_heartbeat_commit = commit_index;
                            last_heartbeat_time = std::time::Instant::now();
                        }
                        
                        // Detect stuck: If no progress for too long, log warning
                        let time_since_last_heartbeat = last_heartbeat_time.elapsed().as_secs();
                        if time_since_last_heartbeat > HEARTBEAT_TIMEOUT_SECS && commit_index == last_heartbeat_commit {
                            warn!("⚠️  [COMMIT PROCESSOR] Possible stuck detected: No progress for {}s (last commit: {})", 
                                time_since_last_heartbeat, commit_index);
                        }
                        
                        next_expected_index += 1;
                        
                        // Process any pending commits that are now in order
                        while let Some(pending) = pending_commits.remove(&next_expected_index) {
                            let pending_commit_index = next_expected_index;
                            
                            let global_exec_index = calculate_global_exec_index(
                                current_epoch,
                                pending_commit_index,
                                last_global_exec_index,
                            );
                            
                            // Process commit normally
                            Self::process_commit(&pending, global_exec_index, current_epoch, executor_client.clone(), pending_transactions_queue.clone()).await?;
                            
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
                    warn!("⚠️  [COMMIT PROCESSOR] Commit receiver closed (commit processor will exit). This is expected during epoch transition.");
                    info!("📊 [COMMIT PROCESSOR] Final stats: processed up to commit #{}", next_expected_index - 1);
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
        _pending_transactions_queue: Option<Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>>,
    ) -> Result<()> {
        let commit_index = subdag.commit_ref.index;
        
        let mut total_transactions = 0;
        let mut transaction_hashes = Vec::new();
        let mut block_details = Vec::new();
        
        // Process blocks in commit order
        for (block_idx, block) in subdag.blocks.iter().enumerate() {
            let transactions = block.transactions();
            let block_tx_count = transactions.len();
            total_transactions += block_tx_count;
            
            // #region agent log
            {
                use std::io::Write;
                let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                    let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:328","message":"PROCESSING BLOCK IN COMMIT - counting all transactions","data":{{"global_exec_index":{},"commit_index":{},"block_idx":{},"block_tx_count":{},"hypothesisId":"B"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                        ts.as_secs(), ts.as_nanos() % 1000000,
                        ts.as_millis(),
                        global_exec_index, commit_index, block_idx, block_tx_count);
                }
            }
            // #endregion
            
            // Calculate official transaction hashes (Keccak256 from TransactionHashData)
            let mut block_tx_hashes = Vec::new();
            
            for (tx_idx, tx) in transactions.iter().enumerate() {
                let tx_data = tx.data();
                let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
                
                // Calculate full hash for general tracking
                use crate::tx_hash::calculate_transaction_hash;
                let tx_hash_full = calculate_transaction_hash(tx_data);
                let tx_hash_full_hex = hex::encode(&tx_hash_full);
                
                // #region agent log - General transaction tracking
                {
                    use std::io::Write;
                    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                        let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:336","message":"TX IN COMMIT - before conversion","data":{{"global_exec_index":{},"commit_index":{},"block_idx":{},"tx_idx":{},"tx_hash":"{}","tx_hash_full":"{}","size":{},"hypothesisId":"B"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                            ts.as_secs(), ts.as_nanos() % 1000000,
                            ts.as_millis(),
                            global_exec_index, commit_index, block_idx, tx_idx, tx_hash_hex, tx_hash_full_hex, tx_data.len());
                    }
                }
                // #endregion
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
            let send_epoch = epoch;
            let send_global_exec_index = global_exec_index;
            
            if let Some(ref client) = executor_client {
                info!("📤 [TX FLOW] Sending committed subdag to Go executor: global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_tx={}", 
                    send_global_exec_index, commit_index, send_epoch, subdag.blocks.len(), total_transactions);
                // #region agent log
                {
                    use std::io::Write;
                    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                        let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:338","message":"BEFORE send_committed_subdag","data":{{"global_exec_index":{},"commit_index":{},"total_tx":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                            ts.as_secs(), ts.as_nanos() % 1000000,
                            ts.as_millis(),
                            send_global_exec_index, commit_index, total_transactions);
                    }
                }
                // #endregion
                if let Err(e) = client.send_committed_subdag(subdag, send_epoch, send_global_exec_index).await {
                    // #region agent log - General transaction tracking
                    {
                        use std::io::Write;
                        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                            let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:340","message":"SEND FAILED - transactions may be lost","data":{{"global_exec_index":{},"commit_index":{},"total_tx":{},"error":"{}","hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                                ts.as_secs(), ts.as_nanos() % 1000000,
                                ts.as_millis(),
                                send_global_exec_index, commit_index, total_transactions, e.to_string().replace("\"", "\\\""));
                        }
                    }
                    // #endregion
                    // CRITICAL FORK-SAFETY: If duplicate global_exec_index detected, this is a serious bug
                    // Log error and skip this commit to prevent fork
                    if e.to_string().contains("Duplicate global_exec_index") {
                        error!("🚨 [DUPLICATE GLOBAL_EXEC_INDEX] Duplicate global_exec_index={} detected in commit processor! This commit will be skipped to prevent fork.", 
                            send_global_exec_index);
                        error!("   📊 Commit details: commit_index={}, epoch={}, total_tx={}", commit_index, send_epoch, total_transactions);
                        error!("   🔍 This indicates a serious bug - same global_exec_index calculated for different commits");
                        error!("   🔍 Possible causes:");
                        error!("      1. global_exec_index calculation is wrong (check calculate_global_exec_index function)");
                        error!("      2. last_global_exec_index was not updated correctly after epoch transition");
                        error!("      3. Commit processor sent same commit twice");
                        error!("      4. Buffer state was not reset properly after restart");
                        
                        // #region agent log
                        {
                            use std::io::Write;
                            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:367","message":"DUPLICATE GLOBAL_EXEC_INDEX - commit skipped","data":{{"global_exec_index":{},"commit_index":{},"total_tx":{},"epoch":{},"hypothesisId":"E"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                                    ts.as_secs(), ts.as_nanos() % 1000000,
                                    ts.as_millis(),
                                    send_global_exec_index, commit_index, total_transactions, send_epoch);
                            }
                        }
                        // #endregion
                        warn!("🚨 [FORK-SAFETY] Duplicate global_exec_index={} detected! This commit will be skipped to prevent fork. Error: {}", 
                            send_global_exec_index, e);
                    } else {
                        warn!("⚠️  [TX FLOW] Failed to send committed subdag to executor: {}", e);
                        // Don't fail commit if executor is unavailable (network issues, etc.)
                    }
                } else {
                    // #region agent log
                    {
                        use std::io::Write;
                        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                            let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:351","message":"SEND SUCCESS","data":{{"global_exec_index":{},"commit_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                                ts.as_secs(), ts.as_nanos() % 1000000,
                                ts.as_millis(),
                                send_global_exec_index, commit_index);
                        }
                    }
                    // #endregion
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
            let send_epoch = epoch;
            let send_global_exec_index = global_exec_index;
            
            if let Some(ref client) = executor_client {
                if let Err(e) = client.send_committed_subdag(subdag, send_epoch, send_global_exec_index).await {
                    // CRITICAL FORK-SAFETY: If duplicate global_exec_index detected, this is a serious bug
                    // Log error and skip this commit to prevent fork
                    if e.to_string().contains("Duplicate global_exec_index") {
                        // #region agent log
                        {
                            use std::io::Write;
                            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"commit_processor.rs:419","message":"DUPLICATE GLOBAL_EXEC_INDEX - empty commit skipped","data":{{"global_exec_index":{},"commit_index":{},"hypothesisId":"E"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                                    ts.as_secs(), ts.as_nanos() % 1000000,
                                    ts.as_millis(),
                                    send_global_exec_index, commit_index);
                            }
                        }
                        // #endregion
                        warn!("🚨 [FORK-SAFETY] Duplicate global_exec_index={} detected! This empty commit will be skipped to prevent fork. Error: {}", 
                            send_global_exec_index, e);
                    } else {
                        warn!("⚠️  Failed to send committed subdag to executor: {}", e);
                        // Don't fail commit if executor is unavailable (network issues, etc.)
                    }
                }
            }
        }
        
        Ok(())
    }
}

