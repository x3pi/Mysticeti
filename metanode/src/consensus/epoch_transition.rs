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

            // [FIX CRITICAL]: Không update provider ở đây.
            // Nếu update trước, đồng hồ đếm giờ của Provider sẽ bị reset.
            // Nếu sau đó Node chuyển đổi thất bại, hệ thống sẽ bị kẹt vì Provider nghĩ rằng đã sang epoch mới.
            
            // Try to get node from global registry and call transition function
            if let Some(node_arc) = crate::node::get_transition_handler_node().await {
                let mut node_guard = node_arc.lock().await;
                
                // Thực hiện chuyển đổi trên Node trước
                if let Err(e) = node_guard.transition_to_epoch_from_system_tx(
                    new_epoch,
                    new_epoch_timestamp_ms,
                    commit_index,
                    &config,
                ).await {
                    error!("❌ [EPOCH TRANSITION HANDLER] Failed to transition epoch: {}", e);
                    // Nếu thất bại: Provider KHÔNG được update. 
                    // Provider sẽ tiếp tục thấy epoch cũ -> tiếp tục bắn System Transaction -> Hệ thống sẽ thử lại (retry).
                } else {
                    info!("✅ [EPOCH TRANSITION HANDLER] Successfully transitioned to epoch {}", new_epoch);
                    
                    // [FIX DONE]: Chỉ update Provider khi Node đã chuyển đổi thành công.
                    // Lúc này mới an toàn để reset đồng hồ cho epoch tiếp theo.
                    system_transaction_provider.update_epoch(
                        new_epoch,
                        new_epoch_timestamp_ms
                    ).await;
                }
            } else {
                warn!("⚠️ [EPOCH TRANSITION HANDLER] Node not registered in global registry yet - transition will be handled when node is available");
                // Không update provider -> Hệ thống sẽ tiếp tục thử lại ở lần check tiếp theo
            }
        }
    });
}