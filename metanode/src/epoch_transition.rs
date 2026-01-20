// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use consensus_core::DefaultSystemTransactionProvider;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{info, error, warn};

use crate::config::NodeConfig;

/// Starts the epoch transition handler task
/// This task processes epoch transition requests from system transactions
pub fn start_epoch_transition_handler(
    mut receiver: UnboundedReceiver<(u64, u64, u32)>,
    system_transaction_provider: Arc<DefaultSystemTransactionProvider>,
    config: NodeConfig,
) {
    tokio::spawn(async move {
        while let Some((new_epoch, new_epoch_timestamp_ms, commit_index)) = receiver.recv().await {
            info!("🚀 [EPOCH TRANSITION HANDLER] Processing transition request: epoch={}, timestamp={}, commit_index={}",
                new_epoch, new_epoch_timestamp_ms, commit_index);

            // Update system transaction provider
            system_transaction_provider.update_epoch(
                new_epoch,
                new_epoch_timestamp_ms
            ).await;

            // Try to get node from global registry and call transition function
            if let Some(node_arc) = crate::node::get_transition_handler_node().await {
                let mut node_guard = node_arc.lock().await;
                if let Err(e) = node_guard.transition_to_epoch_from_system_tx(
                    new_epoch,
                    new_epoch_timestamp_ms,
                    commit_index,
                    &config,
                ).await {
                    error!("❌ [EPOCH TRANSITION HANDLER] Failed to transition epoch: {}", e);
                } else {
                    info!("✅ [EPOCH TRANSITION HANDLER] Successfully transitioned to epoch {}", new_epoch);
                }
            } else {
                warn!("⚠️ [EPOCH TRANSITION HANDLER] Node not registered in global registry yet - transition will be handled when node is available");
                // Transition will be handled when node is registered
            }
        }
    });
}