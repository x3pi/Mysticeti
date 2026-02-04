// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! SyncOnly Block Sync - Rust-Centric Epoch Monitoring
//!
//! Architecture:
//! - Go syncs blocks via network P2P (in network_sync.go)
//! - SyncOnlyNode polls Go for epoch changes (minimal overhead)
//! - When epoch changes detected, triggers epoch transition via channel
//! - CommitProcessor handles transition callback
//!
//! This is a simplified version that delegates block sync to Go
//! and focuses on epoch transition coordination.

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::{ConsensusNode, NodeMode};
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{debug, info, trace, warn};

use crate::node::rust_sync_node::RustSyncHandle;

/// SyncOnly epoch monitor
///
/// Monitors Go for epoch transitions and triggers Rust-side response
/// NOTE: Currently unused - block sync handled by RustSyncNode instead.
/// Kept for potential future reference or alternative sync strategy.
#[allow(dead_code)]
pub struct SyncOnlyNode {
    executor_client: Arc<ExecutorClient>,
    epoch_transition_sender: tokio::sync::mpsc::UnboundedSender<(u64, u64, u64)>,
    current_epoch: Arc<AtomicU64>,
    shared_index: Arc<tokio::sync::Mutex<u64>>,
}

#[allow(dead_code)]
impl SyncOnlyNode {
    /// Create a new SyncOnlyNode
    pub fn new(
        executor_client: Arc<ExecutorClient>,
        epoch_transition_sender: tokio::sync::mpsc::UnboundedSender<(u64, u64, u64)>,
        initial_epoch: u64,
        shared_index: Arc<tokio::sync::Mutex<u64>>,
    ) -> Self {
        Self {
            executor_client,
            epoch_transition_sender,
            current_epoch: Arc::new(AtomicU64::new(initial_epoch)),
            shared_index,
        }
    }

    /// Start monitoring for epoch transitions
    pub fn start(self) -> RustSyncHandle {
        info!(
            "🚀 [SYNC-ONLY] Starting epoch monitor for epoch {}",
            self.current_epoch.load(Ordering::SeqCst)
        );

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let task_handle = tokio::spawn(async move {
            self.run_loop(&mut shutdown_rx).await;
        });

        RustSyncHandle::new(shutdown_tx, task_handle)
    }

    /// Main monitoring loop
    async fn run_loop(self, shutdown_rx: &mut oneshot::Receiver<()>) {
        let mut interval = tokio::time::interval(Duration::from_secs(2));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.poll_once().await;
                }
                _ = &mut *shutdown_rx => {
                    info!("[SYNC-ONLY] Shutdown signal received");
                    return;
                }
            }
        }
    }

    /// Single poll iteration
    async fn poll_once(&self) {
        // Update block index for metrics
        if let Ok(go_last) = self.executor_client.get_last_block_number().await {
            let mut idx = self.shared_index.lock().await;
            if go_last > *idx {
                *idx = go_last;
                trace!("📈 [SYNC-ONLY] Index updated to {}", go_last);
            }
        }

        // Check if Go epoch is ahead
        let current_rust_epoch = self.current_epoch.load(Ordering::SeqCst);
        let go_epoch = match self.executor_client.get_current_epoch().await {
            Ok(e) => e,
            Err(e) => {
                debug!("[SYNC-ONLY] Failed to get epoch: {}", e);
                return;
            }
        };

        if go_epoch > current_rust_epoch {
            info!(
                "🎯 [SYNC-ONLY] Go epoch {} > Rust epoch {} - triggering transition",
                go_epoch, current_rust_epoch
            );

            // Get boundary data and trigger transition
            match self.executor_client.get_epoch_boundary_data(go_epoch).await {
                Ok((epoch, timestamp, boundary_block, _)) => {
                    if let Err(e) =
                        self.epoch_transition_sender
                            .send((epoch, timestamp, boundary_block))
                    {
                        warn!("❌ [SYNC-ONLY] Failed to send transition: {}", e);
                    } else {
                        info!(
                            "✅ [SYNC-ONLY] Sent epoch transition: {} -> {} at block {}",
                            current_rust_epoch, go_epoch, boundary_block
                        );
                        self.current_epoch.store(go_epoch, Ordering::SeqCst);
                    }
                }
                Err(e) => {
                    warn!(
                        "⚠️ [SYNC-ONLY] Cannot get boundary data for epoch {}: {}",
                        go_epoch, e
                    );
                }
            }
        }
    }
}

// =======================
// Legacy API (for compatibility with transition.rs)
// =======================

/// Start the sync task for SyncOnly nodes (using RustSyncNode for P2P sync)
pub async fn start_sync_task(node: &mut ConsensusNode, _config: &NodeConfig) -> Result<()> {
    if !matches!(node.node_mode, NodeMode::SyncOnly) || node.sync_task_handle.is_some() {
        return Ok(());
    }

    info!("🚀 [SYNC TASK] Starting RustSyncNode P2P sync for SyncOnly mode");

    // CRITICAL FIX: Reuse the initialized executor_client from ConsensusNode
    // This client has already run initialize_from_go() and has the correct next_expected_index
    // Creating a new one would reset next_expected_index to 1, causing buffering issues
    let executor_client = node
        .executor_client
        .clone()
        .expect("Executor client must be initialized in ConsensusNode");

    // Load committee from Go
    let (epoch, epoch_timestamp, _boundary_block, validators) = executor_client
        .get_epoch_boundary_data(node.current_epoch)
        .await?;

    // Build committee from validators
    let committee = crate::node::committee::build_committee_from_validator_list(validators, epoch)?;

    info!(
        "🌐 [SYNC TASK] Loaded committee for epoch {} with {} validators",
        epoch,
        committee.size()
    );

    // Create Context for P2P networking
    let sync_metrics = consensus_core::initialise_metrics(prometheus::Registry::new());
    let sync_context = std::sync::Arc::new(consensus_core::Context::new(
        epoch_timestamp,
        consensus_config::AuthorityIndex::new_for_test(0), // SyncOnly uses dummy index
        committee.clone(),
        node.parameters.clone(),
        node.protocol_config.clone(),
        sync_metrics,
        node.clock.clone(),
    ));

    // Start RustSyncNode with full networking
    match crate::node::rust_sync_node::start_rust_sync_task_with_network(
        executor_client,
        node.epoch_transition_sender.clone(),
        epoch,
        0, // initial_commit_index
        sync_context,
        node.network_keypair.clone(),
        committee,
    )
    .await
    {
        Ok(handle) => {
            node.sync_task_handle = Some(handle);
            info!("✅ [SYNC TASK] RustSyncNode P2P sync started successfully");
        }
        Err(e) => {
            warn!("⚠️ [SYNC TASK] Failed to start RustSyncNode: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

/// Stop the sync task (legacy API)
pub async fn stop_sync_task(node: &mut ConsensusNode) -> Result<()> {
    if let Some(handle) = node.sync_task_handle.take() {
        info!("🛑 [SYNC TASK] Stopping...");
        handle.task_handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), handle.task_handle).await;
    }
    Ok(())
}
