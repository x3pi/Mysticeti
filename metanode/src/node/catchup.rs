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

use crate::network::peer_rpc::query_peer_epochs_network;
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
    /// Own Go Master socket (for fallback/identity)
    _own_socket: String,
    /// WAN peer RPC addresses (e.g., "192.168.1.100:19000")
    peer_rpc_addresses: Vec<String>,
    /// Current catchup state
    state: RwLock<CatchupState>,
}

/// Threshold for considering node caught up (within N blocks of network)
const BLOCK_CATCHUP_THRESHOLD: u64 = 5;

impl CatchupManager {
    /// Create new catchup manager
    pub fn new(
        executor_client: Arc<ExecutorClient>,
        own_socket: String,
        peer_rpc_addresses: Vec<String>,
    ) -> Self {
        Self {
            executor_client,
            _own_socket: own_socket,
            peer_rpc_addresses,
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

        // 2. Get Network State from Peers (TCP-based only)
        let (network_epoch, network_block, _best_peer) = if !self.peer_rpc_addresses.is_empty() {
            info!(
                "🌐 [CATCHUP] Using WAN peer discovery ({} peers configured)",
                self.peer_rpc_addresses.len()
            );
            match query_peer_epochs_network(&self.peer_rpc_addresses).await {
                Ok(res) => {
                    info!(
                        "✅ [CATCHUP] WAN peer query success: epoch={}, block={}, peer={}",
                        res.0, res.1, res.2
                    );
                    res
                }
                Err(e) => {
                    warn!(
                        "⚠️ [CATCHUP] WAN peer query failed ({}), using local state",
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
        let epoch_match = local_epoch == network_epoch;

        // Calculate gaps
        let commit_gap = if epoch_match { 0 } else { u64::MAX };

        let block_gap = if epoch_match {
            network_block.saturating_sub(local_go_last_block)
        } else {
            u64::MAX
        };

        // Ready condition: Same Epoch + Block Gap is small
        let ready = epoch_match && block_gap <= BLOCK_CATCHUP_THRESHOLD;

        let status = SyncStatus {
            go_epoch: network_epoch,
            go_last_block: local_go_last_block,
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
                target_commit: 0,
                current_commit: local_last_commit,
            }
        } else {
            CatchupState::Ready
        };

        *self.state.write().await = new_state;

        Ok(status)
    }
}
