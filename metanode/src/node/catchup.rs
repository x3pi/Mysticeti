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
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::node::executor_client::ExecutorClient;

/// State of catchup process
#[derive(Debug, Clone, PartialEq)]
pub enum CatchupState {
    /// Initial state, checking sync status
    Initializing,
    /// Syncing to a different epoch (need to clear old data)
    SyncingEpoch { target_epoch: u64, local_epoch: u64 },
    /// Syncing commits within same epoch
    SyncingCommits {
        target_commit: u64,
        current_commit: u64,
    },
    /// Caught up, ready to participate in consensus
    Ready,
}

/// Result of sync status check
#[derive(Debug)]
pub struct SyncStatus {
    /// Current epoch from Go
    pub go_epoch: u64,
    /// Last executed block from Go
    pub go_last_block: u64,
    /// Local epoch

    /// Whether epoch matches
    pub epoch_match: bool,
    /// Commit gap (how many commits behind)
    pub commit_gap: u64,
    /// Block gap (how many blocks behind)
    pub block_gap: u64,
    /// Network max block height
    pub network_block_height: u64,
    /// Whether ready to join consensus
    pub ready: bool,
}

/// Manager for node catchup synchronization
pub struct CatchupManager {
    /// Client to communicate with Go Master
    executor_client: Arc<ExecutorClient>,
    /// List of peer Go Master sockets to query network state
    peer_sockets: Vec<String>,
    /// Own Go Master socket (for fallback/identity)
    own_socket: String,
    /// Current catchup state
    state: RwLock<CatchupState>,
}

/// Threshold for considering node caught up (within N blocks of network)
const BLOCK_CATCHUP_THRESHOLD: u64 = 5;

impl CatchupManager {
    /// Create new catchup manager
    pub fn new(
        executor_client: Arc<ExecutorClient>,
        peer_sockets: Vec<String>,
        own_socket: String,
    ) -> Self {
        Self {
            executor_client,
            peer_sockets,
            own_socket,
            state: RwLock::new(CatchupState::Initializing),
        }
    }

    /// Check sync status by querying Go Master and Peers
    pub async fn check_sync_status(
        &self,
        local_epoch: u64,
        local_last_commit: u64,
    ) -> Result<SyncStatus> {
        // 1. Get Local State from Go Master
        let local_go_epoch = match self.executor_client.get_current_epoch().await {
            Ok(epoch) => epoch,
            Err(e) => {
                error!("🚨 [CATCHUP] Failed to get current epoch from Go: {}", e);
                return Err(anyhow::anyhow!("Failed to get epoch from Go: {}", e));
            }
        };

        let local_go_last_block = match self.executor_client.get_last_block_number().await {
            Ok(block) => block,
            Err(e) => {
                error!("🚨 [CATCHUP] Failed to get last block from Go: {}", e);
                return Err(anyhow::anyhow!("Failed to get last block from Go: {}", e));
            }
        };

        // 2. Get Network State from Peers
        // If we have peers, query them to find the true "tip" of the chain
        let (network_epoch, network_block, _best_peer) = if !self.peer_sockets.is_empty() {
            match query_peer_epochs(&self.peer_sockets, &self.own_socket).await {
                Ok(res) => res,
                Err(e) => {
                    warn!(
                        "⚠️ [CATCHUP] Failed to query peers, falling back to local state: {}",
                        e
                    );
                    (local_go_epoch, local_go_last_block, "local".to_string())
                }
            }
        } else {
            // No peers configured (single node?), assume we are the network
            (local_go_epoch, local_go_last_block, "local".to_string())
        };

        // 3. Compare States
        // We use network_epoch because if we are behind, we want to catch up to THAT.
        // If local_epoch != network_epoch, we are definitely not ready.
        let epoch_match = local_epoch == network_epoch;

        // Calculate gaps
        let commit_gap = if epoch_match {
            // We don't easily know "network commit index" here without more queries.
            // But existing logic used local_go_last_block as "target" which was WRONG.
            // Let's assume for now we care about BLOCKS.
            0
        } else {
            u64::MAX
        };

        let block_gap = if epoch_match {
            network_block.saturating_sub(local_go_last_block)
        } else {
            u64::MAX
        };

        // Ready condition:
        // 1. Same Epoch
        // 2. Block Gap is small (we have executed most blocks)
        let ready = epoch_match && block_gap <= BLOCK_CATCHUP_THRESHOLD;

        let status = SyncStatus {
            go_epoch: network_epoch,            // Target is network epoch
            go_last_block: local_go_last_block, // Local execution height

            epoch_match,
            commit_gap,
            block_gap,
            network_block_height: network_block,
            ready,
        };

        info!(
            "📊 [CATCHUP] Sync status: Local[Ep={}, Blk={}, Cmt={}] vs Network[Ep={}, Blk={}] -> Gap={} blocks. Ready={}",
            local_epoch, local_go_last_block, local_last_commit,
            network_epoch, network_block,
            if block_gap == u64::MAX { "∞".to_string() } else { block_gap.to_string() },
            ready
        );

        // Update state based on status
        let new_state = if !epoch_match {
            CatchupState::SyncingEpoch {
                target_epoch: network_epoch,
                local_epoch,
            }
        } else if !ready {
            CatchupState::SyncingCommits {
                target_commit: 0, // We rely on blocks now, commit target is vague
                current_commit: local_last_commit,
            }
        } else {
            CatchupState::Ready
        };

        *self.state.write().await = new_state;

        Ok(status)
    }
}

/// Helper function to perform initial catchup at node startup
/// Returns (synced_epoch, synced_last_block)

/// Query multiple Go Master sockets and return the highest epoch found
/// This is used when the local Go Master may have stale data (e.g., after restart)
/// Returns (highest_epoch, last_block_for_that_epoch, socket_path_used)
pub async fn query_peer_epochs(
    peer_sockets: &[String],
    own_socket: &str,
) -> Result<(u64, u64, String)> {
    info!(
        "🔍 [PEER EPOCH] Querying {} peer Go Masters for epoch discovery...",
        peer_sockets.len()
    );

    let mut best_epoch = 0u64;
    let mut best_block = 0u64;
    let mut best_socket = String::new(); // Don't default to own_socket

    // Query each peer socket
    for peer_socket in peer_sockets {
        if peer_socket == own_socket {
            continue; // Skip if same as own
        }

        let peer_client =
            ExecutorClient::new(true, false, String::new(), peer_socket.clone(), None);
        match peer_client.get_current_epoch().await {
            Ok(epoch) => {
                let block = peer_client.get_last_block_number().await.unwrap_or(0);
                info!(
                    "📊 [PEER EPOCH] Peer Go Master ({}): epoch={}, block={}",
                    peer_socket, epoch, block
                );

                // Use this peer if:
                // 1. It has higher epoch
                // 2. Same epoch, but higher block
                // 3. Same epoch/block (arbitrary, but ensures we pick a PEER)
                if best_socket.is_empty()
                    || epoch > best_epoch
                    || (epoch == best_epoch && block > best_block)
                {
                    best_epoch = epoch;
                    best_block = block;
                    best_socket = peer_socket.clone();
                    info!(
                        "✅ [PEER EPOCH] New best peer candidate: epoch={} block={} from {}",
                        epoch, block, peer_socket
                    );
                }
            }
            Err(e) => {
                warn!(
                    "⚠️ [PEER EPOCH] Failed to query peer Go Master ({}): {}",
                    peer_socket, e
                );
            }
        }
    }

    if best_socket.is_empty() {
        // Fallback to local if no peers reachable
        info!("⚠️ [PEER EPOCH] No reachable peers found. Falling back to local Go Master.");
        let own_client =
            ExecutorClient::new(true, false, String::new(), own_socket.to_string(), None);
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
