// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI};
use fastcrypto::hash::{HashFunction, Blake2b256};
use mysten_metrics::monitored_mpsc::UnboundedReceiver;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{info, warn};

use crate::checkpoint::calculate_global_exec_index;

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
}

impl CommitProcessor {
    pub fn new(receiver: UnboundedReceiver<CommittedSubDag>) -> Self {
        Self {
            receiver,
            next_expected_index: 1, // First commit after genesis has index 1
            pending_commits: BTreeMap::new(),
            commit_index_callback: None,
            current_epoch: 0,
            last_global_exec_index: 0,
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

    /// Process commits in order
    pub async fn run(self) -> Result<()> {
        let mut receiver = self.receiver;
        let mut next_expected_index = self.next_expected_index;
        let mut pending_commits = self.pending_commits;
        let commit_index_callback = self.commit_index_callback;
        let current_epoch = self.current_epoch;
        let last_global_exec_index = self.last_global_exec_index;
        
        loop {
            match receiver.recv().await {
                Some(subdag) => {
                    let commit_index: u32 = subdag.commit_ref.index;
                    
                    // If this is the next expected commit, process it immediately
                    if commit_index == next_expected_index {
                        // Calculate deterministic global_exec_index (Checkpoint Sequence Number)
                        let global_exec_index = calculate_global_exec_index(
                            current_epoch,
                            commit_index,
                            last_global_exec_index,
                        );
                        
                        Self::process_commit(&subdag, global_exec_index, current_epoch).await?;
                        
                        // Notify commit index update (for epoch transition)
                        if let Some(ref callback) = commit_index_callback {
                            callback(commit_index);
                        }
                        
                        next_expected_index += 1;
                        
                        // Process any pending commits that are now in order
                        while let Some(pending) = pending_commits.remove(&next_expected_index) {
                            // Calculate deterministic global_exec_index for pending commit
                            let global_exec_index = calculate_global_exec_index(
                                current_epoch,
                                next_expected_index,
                                last_global_exec_index,
                            );
                            
                            Self::process_commit(&pending, global_exec_index, current_epoch).await?;
                            
                            // Notify commit index update
                            if let Some(ref callback) = commit_index_callback {
                                callback(next_expected_index);
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
            
            // Calculate hashes for each transaction
            let mut block_tx_hashes = Vec::new();
            for tx in transactions {
                let tx_data = tx.data();
                let tx_hash = Blake2b256::digest(tx_data).to_vec();
                let tx_hash_hex = hex::encode(&tx_hash[..8]);
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
            
            // TODO: Here you can execute transactions in order with global_exec_index
            // For example:
            // executor.create_block(global_exec_index, subdag).await?;
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
        }
        
        Ok(())
    }
}

