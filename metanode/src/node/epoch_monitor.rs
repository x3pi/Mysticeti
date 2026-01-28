// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Epoch Monitor for SyncOnly nodes
//!
//! Monitors epoch changes and triggers transition to Validator mode
//! when the node is detected in the new committee.

use crate::config::NodeConfig;
use anyhow::Result;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use std::sync::Arc;

/// Start the epoch monitor task for SyncOnly nodes
///
/// This task periodically polls Go's epoch and committee state.
/// When a new epoch is detected and this node is in the committee,
/// it triggers a transition to Validator mode.
pub fn start_epoch_monitor(
    node_mode: crate::node::NodeMode,
    executor_client: &Option<Arc<crate::node::executor_client::ExecutorClient>>,
    current_epoch: u64,
    config: &NodeConfig,
) -> Result<Option<JoinHandle<()>>> {
    // Debug assertions for Send/Sync
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<NodeConfig>();
    assert_sync::<NodeConfig>();
    assert_send::<Arc<crate::node::executor_client::ExecutorClient>>();
    assert_sync::<crate::node::executor_client::ExecutorClient>();
    // Only start for SyncOnly nodes
    if !matches!(node_mode, crate::node::NodeMode::SyncOnly) {
        debug!("📊 [EPOCH MONITOR] Skipping - node is already validator");
        return Ok(None);
    }

    let client_arc = match executor_client {
        Some(client) => client.clone(),
        None => {
            warn!("⚠️ [EPOCH MONITOR] Cannot start - no executor client");
            return Ok(None);
        }
    };

    let node_id = config.node_id;
    let config_clone = config.clone();
    let poll_interval_secs = config.epoch_monitor_poll_interval_secs.unwrap_or(5);

    info!(
        "🔍 [EPOCH MONITOR] Starting epoch monitor for SyncOnly node-{} (current_epoch={}, poll_interval={}s)",
        node_id, current_epoch, poll_interval_secs
    );

    let handle = tokio::spawn(async move {
        let mut last_known_epoch = current_epoch;
        let hostname = format!("node-{}", node_id);

        loop {
            tokio::time::sleep(Duration::from_secs(poll_interval_secs)).await;

            // Check current epoch from Go
            let go_epoch = match client_arc.get_current_epoch().await {
                Ok(e) => e,
                Err(e) => {
                    debug!("🔍 [EPOCH MONITOR] Failed to get epoch from Go: {}", e);
                    continue;
                }
            };

            // If epoch hasn't changed, continue polling
            if go_epoch <= last_known_epoch {
                continue;
            }

            info!(
                "🔄 [EPOCH MONITOR] Detected epoch change: {} -> {}",
                last_known_epoch, go_epoch
            );

            // Epoch changed! Get latest block number first
            let last_block = match client_arc.get_last_block_number().await {
                Ok(b) => b,
                Err(e) => {
                    warn!("⚠️ [EPOCH MONITOR] Failed to get last block number: {}", e);
                    last_known_epoch = go_epoch;
                    continue;
                }
            };

            // Then fetch validators at that block
            let (validators, epoch_timestamp_ms) =
                match client_arc.get_validators_at_block(last_block).await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("⚠️ [EPOCH MONITOR] Failed to get validators: {}", e);
                        last_known_epoch = go_epoch;
                        continue;
                    }
                };

            // Check if our hostname is in the validator list
            let is_in_committee = validators.iter().any(|v| v.name == hostname);

            if is_in_committee {
                info!(
                    "🎉 [EPOCH MONITOR] Node {} is NOW in committee for epoch {}! Triggering transition to Validator mode...",
                    hostname, go_epoch
                );

                // Get the node from global registry and trigger transition
                if let Some(node_arc) = crate::node::get_transition_handler_node().await {
                    let mut node_guard = node_arc.lock().await;

                    // Check if still SyncOnly (might have changed)
                    if !matches!(node_guard.node_mode, crate::node::NodeMode::SyncOnly) {
                        info!("ℹ️ [EPOCH MONITOR] Node already transitioned, stopping monitor");
                        return;
                    }

                    // Trigger the epoch transition
                    // commit_index is 0 since we're joining, not transitioning from previous epoch
                    match node_guard
                        .transition_to_epoch_from_system_tx(
                            go_epoch,
                            epoch_timestamp_ms,
                            0,
                            &config_clone,
                        )
                        .await
                    {
                        Ok(()) => {
                            info!(
                                "✅ [EPOCH MONITOR] Successfully transitioned to Validator mode for epoch {}",
                                go_epoch
                            );
                            // Exit the monitor loop - node is now a validator
                            return;
                        }
                        Err(e) => {
                            warn!(
                                "❌ [EPOCH MONITOR] Failed to transition to Validator mode: {}",
                                e
                            );
                            // Continue monitoring - might succeed on next epoch
                        }
                    }
                } else {
                    warn!("⚠️ [EPOCH MONITOR] Node not found in global registry");
                }
            } else {
                debug!(
                    "📊 [EPOCH MONITOR] Node {} not in committee for epoch {} (found {} validators)",
                    hostname, go_epoch, validators.len()
                );
            }

            last_known_epoch = go_epoch;
        }
    });

    Ok(Some(handle))
}

/// Stop the epoch monitor task
#[allow(dead_code)] // May be called externally or for non-self-triggered transitions
pub async fn stop_epoch_monitor(handle: Option<JoinHandle<()>>) {
    if let Some(h) = handle {
        h.abort();
        info!("🛑 [EPOCH MONITOR] Stopped epoch monitor task");
    }
}
