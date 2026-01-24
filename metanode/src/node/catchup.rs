// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! CatchupManager handles node synchronization when restarting behind the network.
//!
//! When a Rust node restarts, it may be behind other nodes:
//! - Different epoch than the network
//! - Missing committed blocks within same epoch
//!
//! CatchupManager coordinates with:
//! - Go Master: Query current epoch and last executed block
//! - CommitSyncer: Fetch missing commits from peer nodes
//! - ConsensusNode: Control when to join consensus

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn, error};

use crate::node::executor_client::ExecutorClient;

/// State of catchup process
#[derive(Debug, Clone, PartialEq)]
pub enum CatchupState {
    /// Initial state, checking sync status
    Initializing,
    /// Syncing to a different epoch (need to clear old data)
    SyncingEpoch { target_epoch: u64, local_epoch: u64 },
    /// Syncing commits within same epoch
    SyncingCommits { target_commit: u64, current_commit: u64 },
    /// Caught up, ready to participate in consensus
    Ready,
    /// Error state
    Error(String),
}

/// Result of sync status check
#[derive(Debug)]
pub struct SyncStatus {
    /// Current epoch from Go
    pub go_epoch: u64,
    /// Last executed block from Go
    pub go_last_block: u64,
    /// Local epoch
    pub local_epoch: u64,
    /// Local last commit
    pub local_last_commit: u64,
    /// Whether epoch matches
    pub epoch_match: bool,
    /// Commit gap (how many commits behind)
    pub commit_gap: u64,
    /// Whether ready to join consensus
    pub ready: bool,
}

/// Manager for node catchup synchronization
pub struct CatchupManager {
    /// Client to communicate with Go Master
    executor_client: Arc<ExecutorClient>,
    /// Current catchup state
    state: RwLock<CatchupState>,
    /// Whether catchup is in progress
    is_catching_up: AtomicBool,
    /// Threshold for considering node "caught up" (commits behind quorum)
    catchup_threshold: u64,
}

/// Threshold for considering node caught up (within N commits of quorum)
const DEFAULT_CATCHUP_THRESHOLD: u64 = 10;

impl CatchupManager {
    /// Create new catchup manager
    pub fn new(executor_client: Arc<ExecutorClient>) -> Self {
        Self {
            executor_client,
            state: RwLock::new(CatchupState::Initializing),
            is_catching_up: AtomicBool::new(false),
            catchup_threshold: DEFAULT_CATCHUP_THRESHOLD,
        }
    }

    /// Check sync status by querying Go Master
    pub async fn check_sync_status(&self, local_epoch: u64, local_last_commit: u64) -> Result<SyncStatus> {
        // Query Go for current epoch
        let go_epoch = match self.executor_client.get_current_epoch().await {
            Ok(epoch) => epoch,
            Err(e) => {
                error!("🚨 [CATCHUP] Failed to get current epoch from Go: {}", e);
                return Err(anyhow::anyhow!("Failed to get epoch from Go: {}", e));
            }
        };

        // Query Go for last executed block
        let go_last_block = match self.executor_client.get_last_block_number().await {
            Ok(block) => block,
            Err(e) => {
                error!("🚨 [CATCHUP] Failed to get last block from Go: {}", e);
                return Err(anyhow::anyhow!("Failed to get last block from Go: {}", e));
            }
        };

        let epoch_match = local_epoch == go_epoch;
        let commit_gap = if epoch_match {
            go_last_block.saturating_sub(local_last_commit)
        } else {
            // If epochs don't match, gap is significant
            u64::MAX
        };

        let ready = epoch_match && commit_gap <= self.catchup_threshold;

        let status = SyncStatus {
            go_epoch,
            go_last_block,
            local_epoch,
            local_last_commit,
            epoch_match,
            commit_gap,
            ready,
        };

        info!(
            "📊 [CATCHUP] Sync status: go_epoch={}, local_epoch={}, go_last_block={}, local_commit={}, gap={}, ready={}",
            go_epoch, local_epoch, go_last_block, local_last_commit, 
            if commit_gap == u64::MAX { "∞".to_string() } else { commit_gap.to_string() },
            ready
        );

        // Update state based on status
        let new_state = if !epoch_match {
            CatchupState::SyncingEpoch { 
                target_epoch: go_epoch, 
                local_epoch 
            }
        } else if commit_gap > self.catchup_threshold {
            CatchupState::SyncingCommits { 
                target_commit: go_last_block, 
                current_commit: local_last_commit 
            }
        } else {
            CatchupState::Ready
        };

        *self.state.write().await = new_state;

        Ok(status)
    }

    /// Sync to target epoch (clears old epoch data)
    /// Returns the synced epoch
    pub async fn sync_to_epoch(
        &self,
        storage_path: &std::path::Path,
        target_epoch: u64,
        local_epoch: u64,
    ) -> Result<u64> {
        if local_epoch >= target_epoch {
            info!("✅ [CATCHUP] Already at or ahead of target epoch: local={}, target={}", 
                  local_epoch, target_epoch);
            return Ok(local_epoch);
        }

        info!(
            "🔄 [CATCHUP] Syncing epoch: {} -> {} (clearing {} epochs)",
            local_epoch, target_epoch, target_epoch - local_epoch
        );

        self.is_catching_up.store(true, Ordering::SeqCst);

        // Clear all epoch data from local_epoch to target_epoch-1
        // These are stale and we need to sync from peers
        for epoch in local_epoch..target_epoch {
            let epoch_path = storage_path.join("epochs").join(format!("epoch_{}", epoch));
            if epoch_path.exists() {
                info!("🗑️ [CATCHUP] Clearing old epoch data: {:?}", epoch_path);
                if let Err(e) = std::fs::remove_dir_all(&epoch_path) {
                    warn!("⚠️ [CATCHUP] Failed to clear epoch {} data: {}", epoch, e);
                }
            }
        }

        // Update state
        *self.state.write().await = CatchupState::SyncingCommits { 
            target_commit: 0, // Will be updated when we get sync status
            current_commit: 0 
        };

        self.is_catching_up.store(false, Ordering::SeqCst);

        info!("✅ [CATCHUP] Epoch sync complete, now at epoch {}", target_epoch);
        Ok(target_epoch)
    }

    /// Wait until node is caught up to the network
    /// This blocks until either caught up or timeout
    pub async fn wait_for_catchup(&self, local_epoch: u64, timeout_secs: u64) -> Result<bool> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let check_interval = std::time::Duration::from_secs(2);

        info!("⏳ [CATCHUP] Waiting for node to catch up (timeout: {}s)...", timeout_secs);

        loop {
            // Check if we've timed out
            if start.elapsed() > timeout {
                warn!("⚠️ [CATCHUP] Timeout waiting for catchup after {}s", timeout_secs);
                return Ok(false);
            }

            // Get current local commit from shared state (would need to be passed in)
            // For now, re-check sync status
            let status = self.check_sync_status(local_epoch, 0).await?;

            if status.ready {
                info!("✅ [CATCHUP] Node caught up! Ready to participate in consensus.");
                *self.state.write().await = CatchupState::Ready;
                return Ok(true);
            }

            // Log progress
            if !status.epoch_match {
                info!(
                    "🔄 [CATCHUP] Still syncing epoch: local={}, target={}", 
                    status.local_epoch, status.go_epoch
                );
            } else {
                info!(
                    "🔄 [CATCHUP] Syncing commits: {}/{} (gap={})", 
                    status.local_last_commit, status.go_last_block, status.commit_gap
                );
            }

            tokio::time::sleep(check_interval).await;
        }
    }

    /// Check if node is ready to participate in consensus
    pub async fn is_ready(&self) -> bool {
        matches!(*self.state.read().await, CatchupState::Ready)
    }

    /// Check if catchup is in progress
    pub fn is_catching_up(&self) -> bool {
        self.is_catching_up.load(Ordering::SeqCst)
    }

    /// Get current catchup state
    pub async fn get_state(&self) -> CatchupState {
        self.state.read().await.clone()
    }

    /// Mark as ready (called when CommitSyncer catches up)
    pub async fn mark_ready(&self) {
        *self.state.write().await = CatchupState::Ready;
        self.is_catching_up.store(false, Ordering::SeqCst);
        info!("✅ [CATCHUP] Marked as ready");
    }

    /// Update commit progress during sync
    pub async fn update_commit_progress(&self, current_commit: u64, target_commit: u64) {
        let mut state = self.state.write().await;
        if let CatchupState::SyncingCommits { .. } = *state {
            *state = CatchupState::SyncingCommits {
                target_commit,
                current_commit,
            };

            // Check if we've caught up
            if current_commit + self.catchup_threshold >= target_commit {
                *state = CatchupState::Ready;
                self.is_catching_up.store(false, Ordering::SeqCst);
                info!("✅ [CATCHUP] Commit sync complete, ready for consensus");
            }
        }
    }
}

/// Helper function to perform initial catchup at node startup
/// Returns (synced_epoch, synced_last_block)
pub async fn perform_startup_catchup(
    executor_client: &ExecutorClient,
    storage_path: &std::path::Path,
    local_epoch: u64,
) -> Result<(u64, u64)> {
    info!("🚀 [STARTUP CATCHUP] Checking sync status...");

    // Query Go for current state
    let go_epoch = executor_client.get_current_epoch().await
        .map_err(|e| anyhow::anyhow!("Failed to get epoch from Go: {}", e))?;
    
    let go_last_block = executor_client.get_last_block_number().await
        .map_err(|e| anyhow::anyhow!("Failed to get last block from Go: {}", e))?;

    info!(
        "📊 [STARTUP CATCHUP] Go state: epoch={}, last_block={}. Local epoch={}",
        go_epoch, go_last_block, local_epoch
    );

    // Check epoch mismatch
    if local_epoch < go_epoch {
        warn!(
            "🔄 [STARTUP CATCHUP] Epoch mismatch: local={}, go={}. Clearing old epoch data.",
            local_epoch, go_epoch
        );

        // Clear stale epoch data
        for epoch in local_epoch..go_epoch {
            let epoch_path = storage_path.join("epochs").join(format!("epoch_{}", epoch));
            if epoch_path.exists() {
                info!("🗑️ [STARTUP CATCHUP] Clearing stale epoch {} data", epoch);
                if let Err(e) = std::fs::remove_dir_all(&epoch_path) {
                    warn!("⚠️ [STARTUP CATCHUP] Failed to clear epoch {} data: {}", epoch, e);
                }
            }
        }

        info!("✅ [STARTUP CATCHUP] Synced to epoch {}", go_epoch);
        return Ok((go_epoch, go_last_block));
    }

    if local_epoch > go_epoch {
        // Local is ahead - this shouldn't happen but handle gracefully
        error!(
            "🚨 [STARTUP CATCHUP] Local epoch {} is ahead of Go epoch {}! This is unexpected.",
            local_epoch, go_epoch
        );
        // Use Go's epoch as source of truth
        return Ok((go_epoch, go_last_block));
    }

    // Same epoch, return Go's last block
    Ok((go_epoch, go_last_block))
}

/// Query multiple Go Master sockets and return the highest epoch found
/// This is used when the local Go Master may have stale data (e.g., after restart)
/// Returns (highest_epoch, last_block_for_that_epoch, socket_path_used)
pub async fn query_peer_epochs(
    peer_sockets: &[String],
    own_socket: &str,
) -> Result<(u64, u64, String)> {
    info!("🔍 [PEER EPOCH] Querying {} peer Go Masters for epoch discovery...", peer_sockets.len());
    
    let mut best_epoch = 0u64;
    let mut best_block = 0u64;
    let mut best_socket = String::new(); // Don't default to own_socket
    
    // Query each peer socket
    for peer_socket in peer_sockets {
        if peer_socket == own_socket {
            continue; // Skip if same as own
        }
        
        let peer_client = ExecutorClient::new(true, false, String::new(), peer_socket.clone());
        match peer_client.get_current_epoch().await {
            Ok(epoch) => {
                let block = peer_client.get_last_block_number().await.unwrap_or(0);
                info!("📊 [PEER EPOCH] Peer Go Master ({}): epoch={}, block={}", peer_socket, epoch, block);
                
                // Use this peer if:
                // 1. It has higher epoch
                // 2. Same epoch, but higher block
                // 3. Same epoch/block (arbitrary, but ensures we pick a PEER)
                if best_socket.is_empty() 
                   || epoch > best_epoch 
                   || (epoch == best_epoch && block > best_block) {
                    
                    best_epoch = epoch;
                    best_block = block;
                    best_socket = peer_socket.clone();
                    info!("✅ [PEER EPOCH] New best peer candidate: epoch={} block={} from {}", epoch, block, peer_socket);
                }
            }
            Err(e) => {
                warn!("⚠️ [PEER EPOCH] Failed to query peer Go Master ({}): {}", peer_socket, e);
            }
        }
    }
    
    if best_socket.is_empty() {
        // Fallback to local if no peers reachable
        info!("⚠️ [PEER EPOCH] No reachable peers found. Falling back to local Go Master.");
         let own_client = ExecutorClient::new(true, false, String::new(), own_socket.to_string());
        let epoch = own_client.get_current_epoch().await?;
        let block = own_client.get_last_block_number().await.unwrap_or(0);
        return Ok((epoch, block, own_socket.to_string()));
    }
    
    info!(
        "✅ [PEER EPOCH] Best peer found: {} (block={}) from {}",
        best_epoch, best_block, best_socket
    );
    
    Ok((best_epoch, best_block, best_socket))
}
